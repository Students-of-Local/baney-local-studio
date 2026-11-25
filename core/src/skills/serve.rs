use std::future::Future;
use std::path::{Path, PathBuf};

use rmcp::model::{
    CallToolRequestParams, CallToolResponse, CallToolResult, ContentBlock, Implementation, ListToolsResult,
    PaginatedRequestParams, ServerCapabilities, ServerConfig, Tool, ToolAnnotations,
};
use rmcp::service::RequestContext;
use rmcp::{ErrorData, RoleServer, ServerHandler, ServiceExt};
use serde_json::{json, Map, Value};

use super::store::{app, SHIPPED, SHIPPED_DIR};
use super::{lines, plain, roots, Query, Store};

pub const SERVER: &str = "local-studio-skills";
pub(crate) const TOOLS: [&str; 2] = ["skill_search", "skill_read"];
const CLI: &str = if cfg!(windows) { "local-studio-cli.exe" } else { "local-studio-cli" };
const INSTRUCTIONS: &str = "Skills are how-to notes for websites, APIs, apps and jobs. Before a task, call skill_search with it. \
     If one of the skills it lists is about the task, call skill_read for that one and follow it; if none is, work without one. \
     When the message names a skill or a plugin as @name, call skill_read with that name. \
     Search again when a step does not work.";
const CLOSEST: &str =
    "These are the closest, not always a fit. Read one only if it is about this task: skill_read {\"name\": ...}. \
     If none is, work without one.";

pub struct Server {
    pub data: PathBuf,
    pub plugins: Option<PathBuf>,
    pub project: Option<PathBuf>,
    pub shipped: bool,
}

fn tool(name: &'static str, about: &'static str, wanted: &str, asks: &[(&str, &str)]) -> Tool {
    let kind = |name: &str| if name == "from" { "integer" } else { "string" };
    let asks: Map<String, Value> = asks
        .iter()
        .map(|(name, about)| ((*name).to_string(), json!({ "type": kind(name), "description": about })))
        .collect();
    let mut schema = Map::new();
    schema.insert("type".to_string(), json!("object"));
    schema.insert("properties".to_string(), Value::Object(asks));
    schema.insert("required".to_string(), json!([wanted]));
    Tool::new(name, about, schema).with_annotations(ToolAnnotations::new().read_only(true).open_world(false))
}

fn tools() -> Vec<Tool> {
    let search = tool(
        "skill_search",
        "Find how-to notes by what they say: a job, a site, an API, an app, a step you are stuck on. Lists the closest five \
         with their matching parts. Offline, instant. Example: {\"words\":\"find peer-reviewed studies on sleep\"}",
        "words",
        &[
            ("words", "The task or the step you are stuck on, in plain words"),
            ("for", "The site address or app you are on, if any"),
            ("in", "A skill name, to search only its parts"),
        ],
    );
    let read = tool(
        "skill_read",
        "Read one skill listed by skill_search. A long one gives its first part and the names of the others. \
         Example: {\"name\":\"academic-research\"}",
        "name",
        &[
            ("name", "The skill's name, or a plugin's"),
            ("part", "The heading of one part, to read only that"),
            ("from", "The number to read on from, when the answer before was shortened"),
        ],
    );
    vec![search, read]
}

impl Server {
    fn call(&self, tool: &str, given: &Map<String, Value>) -> Result<String, String> {
        let text = |key: &str| given.get(key).and_then(Value::as_str).filter(|text| !text.trim().is_empty());
        let open = || {
            let roots = roots(&self.data, self.plugins.as_deref(), self.project.as_deref())?;
            Store::open(&self.data, if self.shipped { roots.only_shipped() } else { roots })
        };
        match tool {
            "skill_search" => {
                let (words, place, within) = (text("words"), text("for"), text("in"));
                if words.or(place).is_none() {
                    return Err("skill_search needs words: {\"words\": \"the task in plain words\"}".to_string());
                }
                let words = words.unwrap_or_default();
                let hits = open()?.search(&Query { text: words, place, within, limit: 5 })?;
                Ok(match (hits.as_slice(), within) {
                    ([], Some(skill)) => format!("no part of {} says that", plain(skill)),
                    ([], None) => "nothing found. Work without a skill.".to_string(),
                    (hits, Some(_)) => {
                        format!("{}\nRead one part: skill_read {{\"name\": ..., \"part\": ...}}", lines(hits))
                    }
                    (hits, None) => format!("{}\n{CLOSEST}", lines(hits)),
                })
            }
            "skill_read" => {
                let name = text("name").ok_or("skill_read needs a name: {\"name\": \"a skill's name\"}")?;
                let from = match given.get("from") {
                    None | Some(Value::Null) => Some(0),
                    Some(Value::String(from)) => from.trim().parse().ok(),
                    Some(from) => from.as_u64().and_then(|from| usize::try_from(from).ok()),
                };
                let from = from.ok_or("from is a number, such as 6000")?;
                let mut store = open()?;
                let part = text("part");
                let found = store.read(name, part, from);
                let shipped = found.as_ref().is_ok_and(|reading| reading.source == SHIPPED);
                let plugin = store.plugin(name).filter(|_| shipped || found.is_err());
                let Some(plugin) = plugin else { return found.map(|reading| reading.shown()) };
                match plugin.skills.as_slice() {
                    [only] if only.name == name => found.map(|reading| reading.shown()),
                    [only] => store.read(&only.name, part, from).map(|reading| reading.shown()),
                    _ => Ok(super::plugin_block(&plugin, "Read one with skill_read and its name.")),
                }
            }
            _ => Err(format!("no tool named {tool}")),
        }
    }
}

impl ServerHandler for Server {
    fn get_info(&self) -> ServerConfig {
        ServerConfig::new(ServerCapabilities::builder().enable_tools().build())
            .with_server_info(Implementation::new(SERVER, crate::VERSION))
            .with_instructions(INSTRUCTIONS)
    }

    fn list_tools(
        &self,
        _: Option<PaginatedRequestParams>,
        _: RequestContext<RoleServer>,
    ) -> impl Future<Output = Result<ListToolsResult, ErrorData>> + Send + '_ {
        std::future::ready(Ok(ListToolsResult::with_all_items(tools())))
    }

    fn call_tool(
        &self,
        request: CallToolRequestParams,
        _: RequestContext<RoleServer>,
    ) -> impl Future<Output = Result<CallToolResponse, ErrorData>> + Send + '_ {
        let given = request.arguments.unwrap_or_default();
        let result = match self.call(&request.name, &given) {
            Ok(text) => CallToolResult::success(vec![ContentBlock::text(text)]),
            Err(text) => CallToolResult::error(vec![ContentBlock::text(text)]),
        };
        std::future::ready(Ok(result.into()))
    }
}

pub(crate) fn entry(data: &Path, plugins: Option<&Path>, project: Option<&str>) -> Option<Value> {
    let binary = match std::env::var_os("LOCAL_STUDIO_CLI_BIN").filter(|value| !value.is_empty()) {
        Some(binary) => PathBuf::from(binary),
        None => app()?.with_file_name(CLI),
    };
    let whole = |path: &Path| std::path::absolute(path).ok()?.to_str().map(String::from);
    let (binary, data) = (binary.is_file().then(|| whole(&binary))??, whole(data)?);
    let plugins = match plugins {
        Some(plugins) => Some(whole(plugins)?),
        None => None,
    };
    let mut args = vec!["skills", "mcp", "--data", &data];
    if let Some(plugins) = &plugins {
        args.extend(["--plugins", plugins]);
    }
    match project {
        Some(project) => args.extend(["--project", project]),
        None => args.push("--no-project"),
    }
    let mut env = Map::new();
    for name in ["EUKHE_CODING_AGENT_DIR", SHIPPED_DIR] {
        if let Some(dir) = std::env::var_os(name).filter(|dir| !dir.is_empty()) {
            let dir = whole(Path::new(&dir));
            if dir.is_none() && name != SHIPPED_DIR {
                return None;
            }
            env.extend(dir.map(|dir| (name.to_string(), json!(dir))));
        }
    }
    Some(json!({ "command": binary, "args": args, "env": env }))
}

pub fn serve(server: Server) -> Result<(), String> {
    app();
    let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build();
    runtime.map_err(|error| error.to_string())?.block_on(async {
        let running = server.serve(rmcp::transport::stdio()).await.map_err(|error| error.to_string())?;
        running.waiting().await.map(|_| ()).map_err(|error| error.to_string())
    })
}
