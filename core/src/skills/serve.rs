use std::future::Future;
use std::path::Path;
use std::sync::{Mutex, PoisonError};

use rmcp::model::{
    CallToolRequestParams, CallToolResponse, CallToolResult, ContentBlock, Implementation, ListToolsResult,
    PaginatedRequestParams, ServerCapabilities, ServerConfig, Tool, ToolAnnotations,
};
use rmcp::service::RequestContext;
use rmcp::{ErrorData, RoleServer, ServerHandler, ServiceExt};
use serde_json::{json, Map, Value};

use super::{lines, Query, Roots, Store};

pub const SERVER: &str = "local-studio-skills";
const INSTRUCTIONS: &str = "Skills are how-to notes for websites, APIs, apps and jobs. Before a task, call skill_search with it. \
     If one of the skills it lists is about the task, call skill_read for that one and follow it; if none is, work without one. \
     Search again when a step does not work.";
const CLOSEST: &str =
    "These are the closest, not always a fit. Read one only if it is about this task: skill_read {\"name\": ...}. \
     If none is, work without one.";

struct Server(Mutex<Store>);

fn tool(name: &'static str, about: &'static str, wanted: &str, asks: &[(&str, &str)]) -> Tool {
    let asks: Map<String, Value> = asks
        .iter()
        .map(|(name, about)| ((*name).to_string(), json!({ "type": "string", "description": about })))
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
        &[("name", "The skill's name"), ("part", "The heading of one part, to read only that")],
    );
    vec![search, read]
}

impl Server {
    fn call(&self, tool: &str, given: &Map<String, Value>) -> Result<String, String> {
        let text = |key: &str| given.get(key).and_then(Value::as_str).filter(|text| !text.trim().is_empty());
        let mut store = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        match tool {
            "skill_search" => {
                let words = text("words").unwrap_or_default();
                let hits = store.search(&Query { text: words, place: text("for"), within: text("in"), limit: 5 })?;
                Ok(match hits.as_slice() {
                    [] => "nothing found. Work without a skill.".to_string(),
                    hits => format!("{}\n{CLOSEST}", lines(hits)),
                })
            }
            "skill_read" => {
                let name = text("name").ok_or("skill_read needs a name")?;
                store.read(name, text("part")).map(|reading| reading.shown())
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

pub fn serve(data: &Path, roots: Roots) -> Result<(), String> {
    let server = Server(Mutex::new(Store::open(data, roots)?));
    let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build();
    runtime.map_err(|error| error.to_string())?.block_on(async {
        let running = server.serve(rmcp::transport::stdio()).await.map_err(|error| error.to_string())?;
        running.waiting().await.map(|_| ()).map_err(|error| error.to_string())
    })
}
