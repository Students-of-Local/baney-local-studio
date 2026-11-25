use std::io::{IsTerminal, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use local_studio_core::plugins::{self, Capability, Catalog, Consent, Grants, Plugin, Source};
use local_studio_core::tunnel::{self, Cloudflared, Failure, Status, Tunnel};
use local_studio_core::update;
use serde_json::Value;

const USAGE: &str = "usage: local-studio --version
       local-studio import t3 [--dry-run] [--json]
       local-studio plugin validate <dir>
       local-studio plugin list
       local-studio plugin install <dir | catalog id | git url@full sha> [--plugin <name>]
       local-studio plugin grant <id> <capability...>
       local-studio plugin revoke <id> [capability...]
       local-studio plugin capabilities
       local-studio plugin hash <dir> [excluded path...]
       local-studio skills search [--for <site or app>] [--in <skill>] [--limit <n>] [--project <dir>] [--json] <words...>
       local-studio skills read <name> [--part <heading>] [--from <n>] [--project <dir>] [--json]
       local-studio skills list [--project <dir>] [--json]
       local-studio skills mcp [--project <dir>]
       local-studio tunnel status [--json]
       local-studio tunnel setup --relay [<name>] [--account <id>] [--serve <port>] [--mcp <port>] [--yes] [--json]
       local-studio tunnel setup --cloudflare <hostname> [--alias <name>]... [--serve <port>] [--mcp <port>] [--yes] [--json]
       local-studio tunnel setup --tailnet [--serve <port>] [--mcp <port>] [--yes] [--json]
       local-studio tunnel adopt [--hostname <name>] [--yes] [--json]
       local-studio tunnel off [--json]
       local-studio tunnel up [--yes] [--json]
       local-studio tunnel down | release [--json]
       local-studio tunnel ports [--serve <port>] [--mcp <port>] [--json]
       local-studio tunnel cloudflared [--yes] [--json]
       local-studio tunnel byo [--json]
       local-studio tunnel relay run [--serve <port>] [--mcp <port>]
       local-studio fleet status [--json]
       local-studio fleet join [<link>] [--json]
       local-studio fleet invite [--url <wss url>] [--json]
       local-studio fleet share [--active <watts|off>] [--away <watts|max>] [--json]
       local-studio fleet stop-sharing [--json]
       local-studio fleet power-helper [--yes]
       local-studio update check | download | status
       local-studio update apply [--target <path>] [--no-relaunch]";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut args: Vec<&str> = args.iter().map(String::as_str).collect();
    let json = matches!(args.first(), Some(&("tunnel" | "fleet" | "skills"))) && args.contains(&"--json");
    if json {
        args.retain(|arg| *arg != "--json");
    }
    let outcome = match args.as_slice() {
        ["--version"] => {
            println!("local-studio {} ({})", local_studio_core::VERSION, local_studio_core::CHANNEL);
            Ok(())
        }
        ["import", "t3", flags @ ..] if flags.iter().all(|flag| matches!(*flag, "--dry-run" | "--json")) => {
            import_t3(flags.contains(&"--dry-run"), flags.contains(&"--json"))
        }
        ["plugin", "validate", dir] => validate(Path::new(dir)),
        ["plugin", "list"] => list(),
        ["plugin", "install", source] => install(source, None),
        ["plugin", "install", source, "--plugin", name] => install(source, Some(name)),
        ["plugin", "grant", id, names @ ..] if !names.is_empty() => grant(id, names),
        ["plugin", "revoke", id, names @ ..] => revoke(id, names),
        ["plugin", "capabilities"] => {
            for capability in Capability::ALL {
                println!("{}: {}", capability.name(), capability.description());
            }
            Ok(())
        }
        ["plugin", "hash", dir, exclude @ ..] => {
            let exclude: Vec<String> = exclude.iter().map(ToString::to_string).collect();
            plugins::tree_hash(Path::new(dir), &exclude).map(|tree| println!("{tree}"))
        }
        ["skills", verb @ ("search" | "read" | "list" | "mcp"), rest @ ..] => {
            let Some(asked) = skill_args(verb, rest) else {
                eprintln!("{USAGE}");
                return ExitCode::from(2);
            };
            skills(verb, &asked, json)
        }
        ["tunnel", "status"] => run_tunnel(|t| Box::pin(t.status()), json),
        ["tunnel", "setup", flags @ ..] => setup(flags, json),
        ["tunnel", "adopt", flags @ ..] => adopt(flags, json),
        ["tunnel", "off"] => run_state(|t| Box::pin(t.off()), json),
        ["tunnel", "up"] => run_tunnel(|t| Box::pin(t.up(consent(false, json))), json),
        ["tunnel", "up", "--yes"] => run_tunnel(|t| Box::pin(t.up(consent(true, json))), json),
        ["tunnel", "down"] => run_tunnel(|t| Box::pin(t.down()), json),
        ["tunnel", "release"] => run_tunnel(|t| Box::pin(t.release()), json),
        ["tunnel", "ports", flags @ ..] if !flags.is_empty() => {
            given_ports(flags).and_then(|(serve, mcp)| run_tunnel(move |t| Box::pin(t.set_ports(serve, mcp)), json))
        }
        ["tunnel", "cloudflared"] => cloudflared(false, json),
        ["tunnel", "cloudflared", "--yes"] => cloudflared(true, json),
        ["tunnel", "byo"] => {
            show(&tunnel::byo(tunnel::platform()), json);
            Ok(())
        }
        ["tunnel", "relay", "run", flags @ ..] => relay_run(flags, json),
        ["fleet", "status"] => fleet_status(json),
        ["fleet", "join"] => fleet_set(&serde_json::json!({ "discover": true }), json),
        ["fleet", "join", link] => fleet_join(link, json).map_err(|message| fleet_failed(message, json)),
        ["fleet", "invite"] => fleet_invite(None, json).map_err(|message| fleet_failed(message, json)),
        ["fleet", "invite", "--url", url] => {
            fleet_invite(Some((*url).to_string()), json).map_err(|message| fleet_failed(message, json))
        }
        ["fleet", "share", flags @ ..] => share_body(flags).and_then(|body| fleet_set(&body, json)),
        ["fleet", "stop-sharing"] => fleet_set(&serde_json::json!({ "lend": "never" }), json),
        ["fleet", "power-helper"] => power_helper(false),
        ["fleet", "power-helper", "--yes"] => power_helper(true),
        ["update", "check"] => update_check(),
        ["update", "download"] => update_download(),
        ["update", "status"] => update_status(),
        ["update", "apply", flags @ ..] => update_apply(flags),
        _ => {
            eprintln!("{USAGE}");
            return ExitCode::from(2);
        }
    };
    match outcome {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            for line in error.lines() {
                println!("error: {line}");
            }
            ExitCode::FAILURE
        }
    }
}

#[derive(Default)]
struct SkillArgs<'a> {
    place: Option<&'a str>,
    within: Option<&'a str>,
    part: Option<&'a str>,
    from: usize,
    project: Option<&'a str>,
    limit: Option<usize>,
    words: Vec<&'a str>,
}

fn skill_args<'a>(verb: &str, args: &[&'a str]) -> Option<SkillArgs<'a>> {
    let mut asked = SkillArgs::default();
    let mut args = args.iter().copied();
    while let Some(arg) = args.next() {
        match (verb, arg) {
            ("search", "--for") => asked.place = Some(args.next()?),
            ("search", "--in") => asked.within = Some(args.next()?),
            ("search", "--limit") => {
                asked.limit = Some(args.next()?.parse().ok().filter(|limit| (1..=10).contains(limit))?);
            }
            ("read", "--from") => asked.from = args.next()?.parse().ok()?,
            ("read", "--part") => asked.part = Some(args.next()?),
            (_, "--project") => asked.project = Some(args.next()?),
            (_, "--") => asked.words.extend(args.by_ref()),
            (_, flag) if flag.starts_with("--") => return None,
            (_, word) => asked.words.push(word),
        }
    }
    let given = match verb {
        "search" => !asked.words.is_empty() || asked.place.is_some(),
        "read" => !asked.words.is_empty(),
        _ => asked.words.is_empty(),
    };
    given.then_some(asked)
}

fn skills(verb: &str, asked: &SkillArgs, json: bool) -> Result<(), String> {
    use local_studio_core::skills::{lines, plain, roots, serve, Query, Store};
    let data = local_studio_core::data_dir(None)?;
    let plugins = plugins::plugins_dir(None);
    let project = match asked.project {
        Some(dir) => PathBuf::from(dir),
        None => std::env::current_dir().map_err(|error| error.to_string())?,
    };
    if verb == "mcp" {
        return serve(data, plugins, project);
    }
    let mut store = Store::open(&data, roots(&data, plugins.as_deref(), Some(&project))?)?;
    let words = asked.words.join(" ");
    let (value, text) = match verb {
        "search" => {
            let limit = asked.limit.unwrap_or(5);
            let hits = store.search(&Query { text: &words, place: asked.place, within: asked.within, limit })?;
            let text = match hits.as_slice() {
                [] => "nothing found".to_string(),
                hits => {
                    let from = asked.project.map(|dir| format!(" --project {}", plain(dir))).unwrap_or_default();
                    format!("{}\nUse one only if it fits: local-studio skills read{from} <name>", lines(hits))
                }
            };
            (serde_json::to_value(&hits), text)
        }
        "read" => {
            let reading = store.read(&words, asked.part, asked.from)?;
            (serde_json::to_value(&reading), reading.shown())
        }
        _ => {
            let listed = store.list()?;
            let line = |skill: &local_studio_core::skills::Listed| {
                let problem = skill.problem.as_ref().map(|problem| format!(" [{problem}]")).unwrap_or_default();
                let path = format!("{}{problem}", skill.path.display());
                [&skill.name, &skill.source, &path].map(|text| plain(text)).join("\t")
            };
            let text = listed.iter().map(line).collect::<Vec<_>>().join("\n");
            (serde_json::to_value(&listed), text)
        }
    };
    let value = value.map_err(|error| error.to_string())?;
    let shown = |c: char| if c.is_control() { format!("\\u{:04x}", u32::from(c)) } else { c.to_string() };
    if json || !text.is_empty() {
        println!("{}", if json { value.to_string().chars().map(shown).collect() } else { text });
    }
    Ok(())
}

fn import_t3(dry_run: bool, json: bool) -> Result<(), String> {
    use local_studio_core::T3ItemState;
    let report = local_studio_core::import_from_t3(None, dry_run)?;
    if json {
        let text = serde_json::to_string_pretty(&report).map_err(|error| error.to_string())?;
        println!("{text}");
        return Ok(());
    }
    if !report.found {
        for problem in &report.problems {
            println!("{problem}");
        }
        return Ok(());
    }
    let verb = if dry_run { "would import" } else { "imported" };
    println!("T3 Code data: {}", report.source);
    for group in &report.groups {
        let count = |state: T3ItemState| group.items.iter().filter(|item| item.state == state).count();
        let skipped = if group.chosen { "" } else { " (not chosen)" };
        println!(
            "{}{skipped}: {} {verb}, {} already in sync, {} kept as changed in Local Studio, {} not supported",
            group.title,
            count(T3ItemState::Import),
            count(T3ItemState::Synced),
            count(T3ItemState::Kept),
            count(T3ItemState::Unsupported)
        );
        for item in group.items.iter().filter(|item| item.state != T3ItemState::Synced) {
            let mark = match item.state {
                T3ItemState::Import => "+",
                T3ItemState::Kept => "=",
                T3ItemState::Unsupported => "-",
                T3ItemState::Synced => " ",
            };
            if item.detail.is_empty() {
                println!("  {mark} {}", item.label);
            } else {
                println!("  {mark} {}: {}", item.label, item.detail);
            }
        }
    }
    for problem in &report.problems {
        println!("note: {problem}");
    }
    Ok(())
}

fn root() -> Result<PathBuf, String> {
    plugins::plugins_dir(None).ok_or_else(|| "HOME is not set".to_string())
}

fn validate(dir: &Path) -> Result<(), String> {
    let resolved = dir.canonicalize().unwrap_or_else(|_| dir.to_path_buf());
    let plugin = plugins::load(&resolved).map_err(|errors| errors.join("\n"))?;
    println!(
        "OK {} {} ({} skills, {} prompts, {} commands)",
        plugin.id,
        plugin.version,
        plugin.skills.len(),
        plugin.prompts.len(),
        plugin.commands.len()
    );
    asks(&plugin);
    translated(&plugin);
    Ok(())
}

fn translated(plugin: &Plugin) {
    if let Some(translation) = plugins::compat::report(&plugin.dir) {
        println!("  {}", translation.summary());
        for item in &translation.not_used {
            println!("  not used by Local Studio: {item}");
        }
    }
}

fn findings(plugin: &Plugin) {
    let shell = plugins::compat::report(&plugin.dir).map(|translation| translation.findings).unwrap_or_default();
    for finding in shell.iter().chain(&plugins::compat::text_findings(plugin)) {
        println!("  finding: {finding}");
    }
}

fn asks(plugin: &Plugin) {
    if plugin.local_only {
        println!("  {}", plugins::LOCAL_ONLY_LABEL);
    }
    if plugin.capabilities.is_empty() {
        println!("  asks for no capabilities");
    }
    for capability in &plugin.capabilities {
        println!("  asks for {}: {}", capability.name(), capability.description());
    }
    if let Some(mcp) = &plugin.mcp {
        println!("  MCP server {}: {} {}", plugin.mcp_name(), mcp.command.display(), mcp.args.join(" "));
        for (name, host) in &mcp.env {
            println!("  MCP server reads {host} from your environment as {name}");
        }
    }
}

fn consent_line(plugin: &Plugin, grants: &Grants) -> String {
    consent_text(plugin, &grants.consent(plugin))
}

fn consent_text(plugin: &Plugin, consent: &Consent) -> String {
    match consent {
        Consent::Ready => "ready".to_string(),
        Consent::Refused { reason } => format!("refused, so it gets no capabilities: {reason}"),
        Consent::NeedsConsent { missing } => {
            let names: Vec<&str> = missing.iter().map(|capability| capability.name()).collect();
            format!(
                "needs consent for {} (local-studio plugin grant {} {})",
                names.join(", "),
                plugin.id,
                names.join(" ")
            )
        }
    }
}

fn sync(root: &Path) -> Result<(), String> {
    let Some(agent) = plugins::agent_dir() else { return Ok(()) };
    synced(&agent, plugins::sync_agent(root, &agent))
}

fn synced(agent: &Path, changed: Result<bool, String>) -> Result<(), String> {
    if changed? {
        println!("updated mcpServers in {}", agent.join("settings.json").display());
    }
    Ok(())
}

fn list() -> Result<(), String> {
    let root = root()?;
    println!("plugins dir: {}", root.display());
    let scan = plugins::scan(&root);
    let grants = Grants::load(&root)?;
    let ready = plugins::ready(&scan.plugins, &grants);
    for (plugin, consent, _) in &ready {
        println!("{} {}: {}", plugin.id, plugin.version, plugin.description);
        if plugin.local_only {
            println!("  {}", plugins::LOCAL_ONLY_LABEL);
        }
        println!("  {}", consent_text(plugin, consent));
        if let Some((version, source, granted)) =
            grants.granted(&plugin.id).filter(|(_, _, granted)| !granted.is_empty())
        {
            let source = if source.is_empty() { "an earlier Local Studio" } else { source };
            println!("  granted {} at {version} to code from {source}", granted.join(", "));
        }
        for skill in &plugin.skills {
            println!("  skill {} ({})", skill.name, plugin.source());
        }
        for prompt in &plugin.prompts {
            println!("  prompt {} ({})", prompt.name, plugin.source());
        }
        for capability in &plugin.capabilities {
            println!("  capability {}: {}", capability.name(), capability.description());
        }
        translated(plugin);
    }
    for command in plugins::commands(&scan.plugins) {
        println!("command /{} ({}): {}", command.name, command.source, command.description);
    }
    for failure in &scan.failures {
        println!("failed {}:", failure.dir.display());
        for error in &failure.errors {
            println!("  error: {error}");
        }
    }
    let mut errors: Vec<String> = plugins::agent_dir()
        .and_then(|agent| synced(&agent, plugins::sync_mcp_servers(&root, &agent, &ready)).err())
        .into_iter()
        .collect();
    if !scan.failures.is_empty() {
        errors.push(format!("{} plugins failed to load", scan.failures.len()));
    }
    if errors.is_empty() {
        Ok(())
    } else {
        Err(errors.join("\n"))
    }
}

fn install(arg: &str, plugin: Option<&str>) -> Result<(), String> {
    let root = root()?;
    let mut source = Source::parse(arg)?;
    match plugin {
        Some(name) => source = plugins::market::resolve(&source, name)?,
        None => {
            if let Some(entries) = plugins::market::entries(&source)? {
                println!("{arg} is a plugin marketplace; nothing was installed. Its plugins:");
                for entry in entries {
                    println!("  {}: {} (from {})", entry.name, entry.description, entry.from);
                }
                println!("Install one with: local-studio plugin install {arg} --plugin <name>");
                return Ok(());
            }
        }
    }
    let catalog = Catalog::bundled()?;
    let installed = plugins::install(&root, &source, &catalog).map_err(|errors| {
        let mut lines = vec![format!("refused {arg}")];
        lines.extend(errors);
        lines.join("\n")
    })?;
    let plugin = &installed.plugin;
    match &installed.replaced {
        Some(old) => println!("installed {} {} over {old} into {}", plugin.id, plugin.version, plugin.dir.display()),
        None => println!("installed {} {} into {}", plugin.id, plugin.version, plugin.dir.display()),
    }
    println!("  {} files, tree {}, from {}", installed.files, installed.tree, installed.source);
    if let Some(note) = &installed.note {
        println!("  {note}");
    }
    asks(plugin);
    translated(plugin);
    findings(plugin);
    println!("  {}", consent_line(plugin, &Grants::load(&root)?));
    sync(&root)
}

fn installed(root: &Path, id: &str) -> Result<Plugin, String> {
    plugins::load(&root.join(id))
        .map_err(|errors| format!("{id} is not installed or does not load: {}", errors.join("; ")))
}

fn capabilities(names: &[&str]) -> Result<Vec<Capability>, String> {
    names.iter().map(|name| Capability::parse(name).ok_or_else(|| format!("unknown capability: {name}"))).collect()
}

fn grant(id: &str, names: &[&str]) -> Result<(), String> {
    let root = root()?;
    let plugin = installed(&root, id)?;
    let capabilities = capabilities(names)?;
    let _lock = Grants::lock(&root)?;
    let mut grants = Grants::load(&root)?;
    grants.grant(&plugin, &capabilities, None)?;
    grants.save(&root)?;
    println!("{} {}: {}", plugin.id, plugin.version, consent_line(&plugin, &grants));
    sync(&root)
}

fn revoke(id: &str, names: &[&str]) -> Result<(), String> {
    let root = root()?;
    let capabilities = capabilities(names)?;
    let _lock = Grants::lock(&root)?;
    let mut grants = Grants::load(&root)?;
    if !grants.revoke(id, &capabilities) {
        println!("{id}: nothing to revoke");
        return sync(&root);
    }
    grants.save(&root)?;
    match installed(&root, id) {
        Ok(plugin) => println!("{} {}: {}", plugin.id, plugin.version, consent_line(&plugin, &grants)),
        Err(_) => println!("{id}: grants revoked"),
    }
    sync(&root)
}

type TunnelCall<'a> = std::pin::Pin<Box<dyn std::future::Future<Output = Result<Status, Failure>> + 'a>>;

fn failed(failure: &Failure, as_json: bool) -> String {
    if !as_json {
        return failure.message.clone();
    }
    let error =
        serde_json::json!({ "state": "error", "next": "error", "code": failure.code, "message": failure.message });
    println!("{error}");
    String::new()
}

fn run_tunnel(call: impl for<'a> FnOnce(&'a Tunnel) -> TunnelCall<'a>, as_json: bool) -> Result<(), String> {
    let tunnel = Tunnel::open(None).map_err(|error| failed(&error.into(), as_json))?;
    let runtime =
        tokio::runtime::Builder::new_current_thread().enable_all().build().map_err(|error| error.to_string())?;
    let status = runtime.block_on(call(&tunnel)).map_err(|failure| failed(&failure, as_json))?;
    if as_json {
        println!("{}", serde_json::to_string(&status).map_err(|error| error.to_string())?);
        return Ok(());
    }
    println!("{}: {}", status.state, status.message);
    if let Some(position) = status.position {
        println!("  place in line: {position}");
    }
    if let Some(mcp) = &status.mcp_url {
        println!("  MCP address: {mcp}");
    }
    if let Some(ws) = &status.ws_url {
        println!("  phone address: {ws}");
    }
    if let Some(relay) = &status.relay {
        let since = relay.since.and_then(|at| jiff::Timestamp::from_millisecond(at).ok());
        let since = since.map(|at| format!(" since {at:.0}")).unwrap_or_default();
        println!("  relay: {}{since}", relay.state);
    }
    if let Some(command) = status.next_command.as_ref().filter(|command| !status.message.contains(*command)) {
        println!("  next: {command}");
    }
    if let Some(at) = &status.release_at {
        println!("  released on {at} if left down");
    }
    Ok(())
}

type StateCall<'a> = std::pin::Pin<Box<dyn std::future::Future<Output = Result<Value, Failure>> + 'a>>;

fn show(value: &Value, as_json: bool) {
    if as_json {
        println!("{value}");
        return;
    }
    let text = |key: &str| value.get(key).and_then(Value::as_str);
    println!("{}: {}", text("state").unwrap_or("error"), text("message").unwrap_or(""));
    for step in value.get("plan").and_then(Value::as_array).into_iter().flatten().filter_map(Value::as_str) {
        println!("  - {step}");
    }
    for account in value.get("accounts").and_then(Value::as_array).into_iter().flatten() {
        println!("  - {} ({})", account["id"].as_str().unwrap_or(""), account["name"].as_str().unwrap_or(""));
    }
    for (key, label) in [
        ("command", "run"),
        ("nextCommand", "run"),
        ("template", "run"),
        ("url", "open"),
        ("mcpUrl", "MCP address"),
        ("wsUrl", "phone address"),
    ] {
        if let Some(found) = text(key) {
            println!("  {label}: {found}");
        }
    }
}

fn run_state(call: impl for<'a> FnOnce(&'a Tunnel) -> StateCall<'a>, as_json: bool) -> Result<(), String> {
    let tunnel = Tunnel::open(None).map_err(|error| failed(&error.into(), as_json))?;
    let runtime =
        tokio::runtime::Builder::new_current_thread().enable_all().build().map_err(|error| error.to_string())?;
    let value = runtime.block_on(call(&tunnel)).map_err(|failure| failed(&failure, as_json))?;
    show(&value, as_json);
    Ok(())
}

fn port_value(value: Option<&&str>, usage: &str) -> Result<u16, String> {
    value.and_then(|value| value.parse().ok()).ok_or_else(|| usage.to_string())
}

fn setup(flags: &[&str], json: bool) -> Result<(), String> {
    let usage = "usage: local-studio tunnel setup (--relay [<name>] [--account <id>] | --cloudflare <hostname> [--alias <name>]... | --tailnet) [--serve <port>] [--mcp <port>] [--yes] [--json]";
    let (mut cloudflare, mut tailnet, mut relay, mut account, mut yes) = (None, false, None, None, false);
    let (mut aliases, mut serve, mut mcp) = (Vec::new(), None, None);
    let mut rest = flags.iter().peekable();
    while let Some(flag) = rest.next() {
        match *flag {
            "--cloudflare" => cloudflare = Some(rest.next().ok_or(usage)?.to_string()),
            "--relay" => relay = Some(rest.next_if(|name| !name.starts_with("--")).map(ToString::to_string)),
            "--account" => account = Some(rest.next().ok_or(usage)?.to_string()),
            "--alias" => aliases.push(rest.next().ok_or(usage)?.to_string()),
            "--tailnet" => tailnet = true,
            "--yes" => yes = true,
            "--serve" => serve = Some(port_value(rest.next(), usage)?),
            "--mcp" => mcp = Some(port_value(rest.next(), usage)?),
            _ => return Err(usage.to_string()),
        }
    }
    match (cloudflare, tailnet, relay) {
        (None, false, Some(name)) if aliases.is_empty() => setup_relay(name, account, (serve, mcp), yes, json),
        _ if account.is_some() => Err(usage.to_string()),
        (Some(host), false, None) => {
            run_state(move |t| Box::pin(t.setup_cloudflare(host, aliases, serve, mcp, consent(yes, json))), json)
        }
        (None, true, None) if aliases.is_empty() => run_state(move |t| Box::pin(t.setup_tailnet(serve, mcp)), json),
        _ => Err(usage.to_string()),
    }
}

#[cfg(feature = "link")]
fn setup_relay(
    name: Option<String>,
    account: Option<String>,
    ports: (Option<u16>, Option<u16>),
    yes: bool,
    as_json: bool,
) -> Result<(), String> {
    let go = move |state: &Value| {
        if yes || as_json || !std::io::stdin().is_terminal() {
            return yes;
        }
        println!("{}", state["message"].as_str().unwrap_or_default());
        print!("Replace it? [y/N] ");
        let _ = std::io::stdout().flush();
        let mut answer = String::new();
        std::io::stdin().read_line(&mut answer).is_ok() && matches!(answer.trim(), "y" | "Y" | "yes" | "Yes")
    };
    run_state(move |t| Box::pin(t.setup_relay(name, account, ports.0, ports.1, go)), as_json)
}

#[cfg(not(feature = "link"))]
fn setup_relay(
    _name: Option<String>,
    _account: Option<String>,
    _ports: (Option<u16>, Option<u16>),
    _yes: bool,
    _as_json: bool,
) -> Result<(), String> {
    Err(NO_RELAY.into())
}

fn adopt(flags: &[&str], json: bool) -> Result<(), String> {
    let usage = "usage: local-studio tunnel adopt [--hostname <name>] [--yes] [--json]";
    let (mut hostname, mut yes) = (None, false);
    let mut rest = flags.iter();
    while let Some(flag) = rest.next() {
        match *flag {
            "--hostname" => hostname = Some(rest.next().ok_or(usage)?.to_string()),
            "--yes" => yes = true,
            _ => return Err(usage.to_string()),
        }
    }
    let go = move |plan: &Value| {
        if yes {
            return true;
        }
        if json || !std::io::stdin().is_terminal() {
            return false;
        }
        for step in plan.get("plan").and_then(Value::as_array).into_iter().flatten().filter_map(Value::as_str) {
            println!("  - {step}");
        }
        print!("Adopt Tychicus's tunnel this way? [y/N] ");
        let _ = std::io::stdout().flush();
        let mut answer = String::new();
        std::io::stdin().read_line(&mut answer).is_ok() && matches!(answer.trim(), "y" | "Y" | "yes" | "Yes")
    };
    run_state(move |t| Box::pin(t.adopt(hostname, go, consent(yes, json))), json)
}

fn given_ports(flags: &[&str]) -> Result<(Option<u16>, Option<u16>), String> {
    let usage = "usage: local-studio tunnel (ports | relay run) [--serve <port>] [--mcp <port>]";
    let (mut serve, mut mcp) = (None, None);
    let mut rest = flags.iter();
    while let Some(flag) = rest.next() {
        match *flag {
            "--serve" => serve = Some(port_value(rest.next(), usage)?),
            "--mcp" => mcp = Some(port_value(rest.next(), usage)?),
            _ => return Err(usage.to_string()),
        }
    }
    Ok((serve, mcp))
}

#[cfg(feature = "link")]
fn relay_run(flags: &[&str], as_json: bool) -> Result<(), String> {
    let (serve, mcp) = given_ports(flags)?;
    let tunnel = Tunnel::open(None).map_err(|error| failed(&error.into(), as_json))?;
    let runtime =
        tokio::runtime::Builder::new_multi_thread().enable_all().build().map_err(|error| error.to_string())?;
    let log = |line: &str| eprintln!("local-studio tunnel relay: {line}");
    let ran = runtime.block_on(tunnel.relay_run(serve, mcp, log)).map_err(|failure| failed(&failure, as_json));
    runtime.shutdown_background();
    ran
}

#[cfg(not(feature = "link"))]
const NO_RELAY: &str = "this build has no relay: build local-studio with --features link";

#[cfg(not(feature = "link"))]
fn relay_run(_flags: &[&str], _as_json: bool) -> Result<(), String> {
    Err(NO_RELAY.into())
}

fn consent(yes: bool, as_json: bool) -> impl FnOnce(&Path, &[String]) -> bool {
    move |target, notes| {
        if yes {
            return true;
        }
        if as_json || !std::io::stdin().is_terminal() {
            return false;
        }
        for note in notes {
            println!("{note}");
        }
        print!(
            "Download cloudflared {} from github.com/cloudflare/cloudflared, check its pinned SHA-256, and install it at {}? [y/N] ",
            tunnel::CLOUDFLARED_VERSION,
            target.display()
        );
        let _ = std::io::stdout().flush();
        let mut answer = String::new();
        std::io::stdin().read_line(&mut answer).is_ok() && matches!(answer.trim(), "y" | "Y" | "yes" | "Yes")
    }
}

fn cloudflared(yes: bool, as_json: bool) -> Result<(), String> {
    let tunnel = Tunnel::open(None).map_err(|error| failed(&error.into(), as_json))?;
    let runtime =
        tokio::runtime::Builder::new_current_thread().enable_all().build().map_err(|error| error.to_string())?;
    let found = runtime
        .block_on(tunnel.cloudflared(consent(yes, as_json), "local-studio tunnel cloudflared --yes"))
        .map_err(|failure| failed(&failure, as_json))?;
    if as_json {
        let pinned = matches!(found, Cloudflared::Pinned(_));
        let path = found.path().display().to_string();
        let version = tunnel::CLOUDFLARED_VERSION;
        println!(
            "{}",
            serde_json::json!({ "state": "ready", "next": "ready", "path": path, "pinned": pinned, "version": version })
        );
        return Ok(());
    }
    match found {
        Cloudflared::Dev(path) => println!("LS_CLOUDFLARED: {} (a dev override, not checked)", path.display()),
        Cloudflared::Pinned(path) => {
            println!("cloudflared {} at {} (pinned digest checked)", tunnel::CLOUDFLARED_VERSION, path.display());
        }
    }
    Ok(())
}

fn update_runtime() -> Result<tokio::runtime::Runtime, String> {
    tokio::runtime::Builder::new_current_thread().enable_all().build().map_err(|error| error.to_string())
}

fn update_data_dir() -> Result<PathBuf, String> {
    local_studio_core::data_dir(None)
}

fn update_check() -> Result<(), String> {
    let build = update::Build::current();
    println!("{} {} ({}), feed {}", update::channel_name(&build.channel), build.version, build.channel, build.feed);
    match update_runtime()?.block_on(update::check(&build)).map_err(|refusal| refusal.to_string())? {
        update::Checked::UpToDate { feed } => println!("up to date (the feed holds {feed})"),
        update::Checked::Available(offer) => {
            println!("available: {} ({})", offer.version, offer.asset);
            for line in offer.notes.lines() {
                println!("  {line}");
            }
        }
    }
    Ok(())
}

fn update_download() -> Result<(), String> {
    let build = update::Build::current();
    let runtime = update_runtime()?;
    let offer = match runtime.block_on(update::check(&build)).map_err(|refusal| refusal.to_string())? {
        update::Checked::UpToDate { feed } => {
            println!("up to date (the feed holds {feed})");
            return Ok(());
        }
        update::Checked::Available(offer) => offer,
    };
    let data_dir = update_data_dir()?;
    let mut shown = 0;
    let staged = runtime
        .block_on(update::download(&build, &offer, &data_dir, |received, total| {
            let mib = received >> 20;
            if mib >= shown + 16 || total == Some(received) {
                shown = mib;
                match total {
                    Some(total) => eprintln!("downloaded {mib} of {} MiB", total >> 20),
                    None => eprintln!("downloaded {mib} MiB"),
                }
            }
        }))
        .map_err(|refusal| refusal.to_string())?;
    println!("ready: {} at {}", staged.version, staged.artifact.display());
    Ok(())
}

fn update_status() -> Result<(), String> {
    let build = update::Build::current();
    println!("{} {} ({})", update::channel_name(&build.channel), build.version, build.channel);
    if let Some(refusal) = build.refusal() {
        println!("{refusal}");
        return Ok(());
    }
    match update::staged(&build, &update_data_dir()?) {
        Some(staged) => println!("ready: {} at {}", staged.version, staged.artifact.display()),
        None => println!("nothing downloaded"),
    }
    Ok(())
}

fn update_apply(flags: &[&str]) -> Result<(), String> {
    let mut target = None;
    let mut relaunch = true;
    let mut rest = flags.iter();
    while let Some(flag) = rest.next() {
        match *flag {
            "--target" => target = Some(PathBuf::from(rest.next().ok_or_else(|| USAGE.to_string())?)),
            "--no-relaunch" => relaunch = false,
            _ => return Err(USAGE.to_string()),
        }
    }
    let build = update::Build::current();
    if let Some(refusal) = build.refusal() {
        return Err(refusal.to_string());
    }
    let staged = update::staged(&build, &update_data_dir()?)
        .ok_or_else(|| "nothing is downloaded; run local-studio update download first".to_string())?;
    let target = match target {
        Some(target) => target,
        None => update::current_install().or_else(|_| {
            update::install_path(&build.channel).ok_or_else(|| "no install path on this platform".to_string())
        })?,
    };
    let applied = update::apply(&build, &staged, &target).map_err(|refusal| refusal.to_string())?;
    let _ = std::fs::remove_dir_all(&staged.dir);
    println!(
        "installed {} at {} (previous build kept at {})",
        applied.version,
        applied.target.display(),
        applied.previous.display()
    );
    if relaunch {
        update::relaunch(&applied.target)?;
    }
    Ok(())
}

struct Fleet {
    base: String,
    key: Option<String>,
}

fn json_text(value: &Value, key: &str) -> Option<String> {
    value.get(key).and_then(Value::as_str).map(str::trim).filter(|text| !text.is_empty()).map(str::to_string)
}

fn fleet_open() -> Result<Fleet, String> {
    let data = local_studio_core::data_dir(None)?;
    Ok(Fleet { base: local_studio_core::fleet::controller_url(&data), key: local_studio_core::fleet::key(&data) })
}

fn fleet_call(body: Option<&Value>) -> Result<Value, String> {
    controller_call("/api/cluster", body)
}

fn controller_call(path: &str, body: Option<&Value>) -> Result<Value, String> {
    let fleet = fleet_open()?;
    let runtime =
        tokio::runtime::Builder::new_current_thread().enable_all().build().map_err(|error| error.to_string())?;
    runtime.block_on(async {
        let http = reqwest::Client::builder()
            .connect_timeout(std::time::Duration::from_secs(3))
            .timeout(std::time::Duration::from_secs(30))
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(|error| error.to_string())?;
        let url = format!("{}{path}", fleet.base);
        let mut request = match body {
            Some(body) => http.post(&url).json(body),
            None => http.get(&url),
        };
        if let Some(key) = &fleet.key {
            request = request.bearer_auth(key);
        }
        let response = request.send().await.map_err(|error| {
            if error.is_connect() {
                format!(
                    "No Local controller answers at {}. Open Local Studio (it starts the controller) or start the controller, then try again.",
                    fleet.base
                )
            } else {
                format!("The Local controller at {} did not answer: {error}", fleet.base)
            }
        })?;
        let status = response.status();
        let value: Value = response.json().await.unwrap_or(Value::Null);
        if status == reqwest::StatusCode::UNAUTHORIZED {
            return Err(match fleet.key {
                Some(_) => format!("The controller at {} refused this machine's fleet key.", fleet.base),
                None => format!(
                    "The controller at {} needs the fleet key, and none was found in Local Studio's settings or the controller's config.json.",
                    fleet.base
                ),
            });
        }
        if !status.is_success() {
            let message = value
                .get("error")
                .and_then(|error| error.get("message"))
                .and_then(Value::as_str)
                .map_or_else(|| format!("controller error {status}"), str::to_string);
            return Err(message);
        }
        Ok(value)
    })
}

fn fleet_failed(message: String, as_json: bool) -> String {
    if !as_json {
        return message;
    }
    println!("{}", serde_json::json!({ "state": "error", "message": message }));
    String::new()
}

#[cfg(feature = "link")]
fn fleet_invite(url: Option<String>, as_json: bool) -> Result<(), String> {
    use local_studio_core::link::{devices, pairing};
    let data = local_studio_core::data_dir(None)?;
    if let Some(url) = &url {
        if !url.starts_with("wss://") && !url.starts_with("ws://127.0.0.1:") {
            return Err(format!("--url must be a wss:// address (or ws://127.0.0.1 for a test), not {url}"));
        }
    }
    // Off the tailnet the other machine reaches this one through its public route, so that comes first.
    let url = url.or_else(|| public_route_url(&data)).or_else(pairing::tailscale_url).ok_or_else(|| {
        "No public route or tailnet name was found, so the other machine could not reach this one. Set one up with `local-studio tunnel setup`, or pass `--url wss://...`.".to_string()
    })?;
    let minted = pairing::mint(&data, local_studio_core::CHANNEL, Some(url), None, &[devices::FLEET])
        .map_err(|error| format!("Could not mint a fleet pairing: {error:#}"))?;
    if as_json {
        println!(
            "{}",
            serde_json::json!({
                "state": "minted",
                "link": minted.link,
                "url": minted.url,
                "expiresAt": minted.expires_at,
                "scopes": minted.scopes,
            })
        );
    } else {
        println!("{}", minted.link);
        println!(
            "Run `local-studio fleet join <link>` on the other machine within 10 minutes. The link works once and lets that machine use this one's models through the fleet."
        );
    }
    Ok(())
}

// The hosted or Cloudflare route's hostname from <data dir>/tunnel/route.json (docs/tunnel.md), as a link URL.
#[cfg(feature = "link")]
fn public_route_url(data: &std::path::Path) -> Option<String> {
    let route: Value = serde_json::from_slice(&std::fs::read(data.join("tunnel").join("route.json")).ok()?).ok()?;
    let public = matches!(route.get("route").and_then(Value::as_str), Some("cloudflare" | "hosted"));
    let host = route.get("hostname").and_then(Value::as_str).filter(|host| {
        !host.is_empty() && host.bytes().all(|byte| byte.is_ascii_alphanumeric() || byte == b'.' || byte == b'-')
    })?;
    public.then(|| format!("wss://{host}/v1/ws"))
}

#[cfg(feature = "link")]
fn fleet_join(link: &str, as_json: bool) -> Result<(), String> {
    use local_studio_core::link::{devices, pairing, proxy};
    let data = local_studio_core::data_dir(None)?;
    let parsed =
        pairing::parse(link, local_studio_core::CHANNEL).map_err(|message| format!("Could not pair: {message}"))?;
    controller_call("/api/node", None)?;
    if !data_dir_owned(&data) {
        return Err(format!(
            "Nothing owns {} right now. Open Local Studio or start `local-studio-serve serve`: the process that owns the data folder keeps the link open and runs the fleet proxy. Then run this again.",
            data.display()
        ));
    }
    let folder = data.join("remote").join("requests");
    std::fs::create_dir_all(&folder).map_err(|error| format!("create {}: {error}", folder.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&folder, std::fs::Permissions::from_mode(0o700))
            .map_err(|error| format!("protect {}: {error}", folder.display()))?;
    }
    let id = devices::random::<16>().map(|bytes| devices::hex(&bytes)).map_err(|error| error.to_string())?;
    let request = folder.join(format!("{id}.json"));
    let asked = serde_json::json!({ "op": "fleet_join", "link": link.trim() });
    devices::write_private(&request, asked.to_string().as_bytes()).map_err(|error| format!("{error:#}"))?;
    let done = folder.join(format!("{id}.done.json"));
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
    let answer: Value = loop {
        if let Ok(bytes) = std::fs::read(&done) {
            std::fs::remove_file(&done).ok();
            break serde_json::from_slice(&bytes).map_err(|error| format!("unreadable answer: {error}"))?;
        }
        if std::time::Instant::now() >= deadline {
            std::fs::remove_file(&request).ok();
            return Err(format!(
                "The process that owns {} has not answered within 30 s, so this request was withdrawn. If it had already started pairing with {}, it finishes and adds the machine to the fleet itself. The app answers only while it serves (serve.json); otherwise run this again with the same invite while it is still valid.",
                data.display(),
                parsed.name
            ));
        }
        std::thread::sleep(std::time::Duration::from_millis(200));
    };
    if answer.get("ok").and_then(Value::as_bool) != Some(true) {
        return Err(json_text(&answer, "message").unwrap_or_else(|| "the pairing failed".to_string()));
    }
    let machine = answer.get("machine").cloned().unwrap_or(Value::Null);
    let machine_id = json_text(&machine, "id").ok_or("the owner answered without a machine id")?;
    let name = json_text(&machine, "name").unwrap_or_else(|| machine_id.clone());
    let peer = proxy::peer_url(&machine_id);
    if as_json {
        println!(
            "{}",
            serde_json::json!({ "state": "joined", "machine": { "id": machine_id, "name": name }, "peer": peer })
        );
    } else {
        println!("Joined {name}: its models are in this machine's fleet through {peer}.");
    }
    Ok(())
}

#[cfg(feature = "link")]
fn data_dir_owned(data: &Path) -> bool {
    let Ok(file) = std::fs::OpenOptions::new().read(true).write(true).open(data.join("studio.lock")) else {
        return false;
    };
    matches!(file.try_lock(), Err(std::fs::TryLockError::WouldBlock))
}

#[cfg(not(feature = "link"))]
fn fleet_join(_link: &str, _as_json: bool) -> Result<(), String> {
    Err("This local-studio was built without desktop links (the core's link feature), so it cannot join a fleet over a link.".to_string())
}

#[cfg(not(feature = "link"))]
fn fleet_invite(_url: Option<String>, _as_json: bool) -> Result<(), String> {
    Err("This local-studio was built without desktop links (the core's link feature), so it cannot mint a fleet pairing. Use `local-studio-serve pair --fleet`.".to_string())
}

fn fleet_status(as_json: bool) -> Result<(), String> {
    let value = fleet_call(None).map_err(|message| fleet_failed(message, as_json))?;
    fleet_show(&value, as_json);
    Ok(())
}

fn fleet_set(body: &Value, as_json: bool) -> Result<(), String> {
    let value = fleet_call(Some(body)).map_err(|message| fleet_failed(message, as_json))?;
    fleet_show(&value, as_json);
    Ok(())
}

fn watts_flag(raw: &str, word: &str, none: Value, least: u32) -> Result<Value, String> {
    if raw == word {
        return Ok(none);
    }
    match raw.parse::<u32>() {
        Ok(watts) if (least..=2000).contains(&watts) => Ok(Value::from(watts)),
        _ => Err(format!("{raw} is not a wattage: use a whole number from {least} to 2000, or {word}")),
    }
}

fn share_body(flags: &[&str]) -> Result<Value, String> {
    let mut power = serde_json::Map::new();
    let mut rest = flags.iter();
    while let Some(flag) = rest.next() {
        let value = rest.next().ok_or_else(|| USAGE.to_string())?;
        match *flag {
            "--active" => power.insert("activeWatts".into(), watts_flag(value, "off", Value::from(0), 0)?),
            "--away" => power.insert("awayWatts".into(), watts_flag(value, "max", Value::Null, 1)?),
            _ => return Err(USAGE.to_string()),
        };
    }
    let mut body = serde_json::json!({ "lend": "idle" });
    if !power.is_empty() {
        body["power"] = Value::Object(power);
    }
    Ok(body)
}

fn watts_text(value: Option<&Value>, zero: &str, null: &str) -> String {
    match value.and_then(Value::as_f64) {
        None => null.to_string(),
        Some(_) if value.and_then(Value::as_u64) == Some(0) => zero.to_string(),
        Some(watts) => format!("up to {watts} W"),
    }
}

fn fleet_show(value: &Value, as_json: bool) {
    let power = value.get("power").cloned().unwrap_or(Value::Null);
    let machines = value.get("machines").and_then(Value::as_array).cloned().unwrap_or_default();
    if as_json {
        let peers: Vec<Value> = machines
            .iter()
            .map(|machine| {
                serde_json::json!({
                    "id": machine.get("id"), "name": machine.get("name"), "url": machine.get("url"),
                    "state": machine.get("state"), "lend": machine.get("lend"), "reason": machine.get("reason"),
                })
            })
            .collect();
        let out = serde_json::json!({
            "state": value.get("state"), "lend": value.get("lend"), "discover": value.get("discover"),
            "reason": value.get("reason"), "since": value.get("since"), "checks": value.get("checks"),
            "power": power, "peers": peers,
        });
        println!("{out}");
        return;
    }
    let text = |from: &Value, key: &str| json_text(from, key).unwrap_or_default();
    println!("{}: {}", text(value, "state"), text(value, "reason"));
    println!(
        "  sharing: {}, finding other machines: {}",
        text(value, "lend"),
        if value.get("discover").and_then(Value::as_bool) == Some(true) { "on" } else { "off" }
    );
    println!(
        "  power while in use: {}; while away: {}",
        watts_text(power.get("activeWatts"), "does not lend", "no cap"),
        watts_text(power.get("awayWatts"), "up to 1 W", "no cap")
    );
    let owner = match power.get("present").and_then(Value::as_bool) {
        Some(true) => "present",
        Some(false) => "away",
        None => "unknown",
    };
    println!("  owner: {owner} ({})", text(&power, "presenceSource"));
    match power.get("capW").and_then(Value::as_f64) {
        Some(cap) => println!("  cap now: {cap} W through {}", text(&power, "lever")),
        None => println!("  cap now: none"),
    }
    match power.get("measuredW").and_then(Value::as_f64) {
        Some(draw) => println!("  fleet draw: {draw} W"),
        None => println!("  fleet draw: not measured"),
    }
    println!("  power helper: {}", text(&power, "helper"));
    for check in value.get("checks").and_then(Value::as_array).into_iter().flatten() {
        let ok = check.get("ok").and_then(Value::as_bool) == Some(true);
        println!("  {} {}: {}", if ok { "pass" } else { "fail" }, text(check, "name"), text(check, "detail"));
    }
    if machines.is_empty() {
        println!("  peers: none yet");
    }
    for machine in &machines {
        println!(
            "  peer {} ({}): {}, {}",
            text(machine, "name"),
            text(machine, "url"),
            text(machine, "state"),
            text(machine, "reason")
        );
    }
}

const SUDOERS_PATH: &str = "/etc/sudoers.d/local-studio-power";

fn on_path(program: &str) -> Option<PathBuf> {
    std::env::split_paths(&std::env::var_os("PATH")?).map(|dir| dir.join(program)).find(|path| path.is_file())
}

fn sudoers_text(user: &str, nvidia_smi: &Path) -> String {
    let digits = |count: usize| "[0-9]".repeat(count);
    let mut watts: Vec<String> = (1..=4).map(digits).collect();
    watts.extend((1..=4).map(|count| format!("{}.{}", digits(count), digits(2))));
    let lines: Vec<String> = [digits(1), digits(2)]
        .iter()
        .flat_map(|index| watts.iter().map(move |watt| format!("{} -i {index} -pl {watt}", nvidia_smi.display())))
        .collect();
    format!(
        "Cmnd_Alias LOCAL_STUDIO_POWER = \\\n    {}\n{user} ALL=(root) NOPASSWD: LOCAL_STUDIO_POWER\n",
        lines.join(", \\\n    ")
    )
}

/// The nvidia-smi the sudoers rule may name: the first one on PATH, resolved through symlinks, and only when
/// root owns it and every directory above it and no other user or group can write any of them. A rule for a
/// file the user could replace would hand root to anything running as the user.
#[cfg(unix)]
fn trusted_nvidia_smi(found: &Path) -> Result<PathBuf, String> {
    use std::os::unix::fs::{MetadataExt, PermissionsExt};
    let real =
        std::fs::canonicalize(found).map_err(|error| format!("could not resolve {}: {error}", found.display()))?;
    let meta = std::fs::metadata(&real).map_err(|error| format!("could not read {}: {error}", real.display()))?;
    if !meta.is_file() || meta.permissions().mode() & 0o111 == 0 {
        return Err(format!("{} is not an executable file, so no sudoers rule is written.", real.display()));
    }
    for path in real.ancestors() {
        let meta = std::fs::metadata(path).map_err(|error| format!("could not read {}: {error}", path.display()))?;
        if meta.uid() != 0 || meta.mode() & 0o022 != 0 {
            let which = if path == real { "that file".to_string() } else { path.display().to_string() };
            return Err(format!(
                "nvidia-smi resolves to {}, and {which} is not owned by root or can be changed by another user or group. A sudoers rule for it would let any program running as you become root, so none is written. Install the NVIDIA driver's nvidia-smi (usually /usr/bin/nvidia-smi) and put its directory first in PATH.",
                real.display()
            ));
        }
    }
    Ok(real)
}

#[cfg(not(unix))]
fn trusted_nvidia_smi(found: &Path) -> Result<PathBuf, String> {
    Err(format!("{} can't be checked on this system, so no sudoers rule is written.", found.display()))
}

fn power_helper(yes: bool) -> Result<(), String> {
    if !cfg!(target_os = "linux") {
        return Err("The power helper is only for Linux machines with an NVIDIA GPU. On this machine the controller caps fleet work by pausing it in short bursts, which needs no setup.".to_string());
    }
    let found = on_path("nvidia-smi").ok_or(
        "This machine has no NVIDIA GPU (nvidia-smi is not installed), so there is no power limit to set. The controller caps fleet work by pausing it instead.",
    )?;
    let nvidia_smi = trusted_nvidia_smi(&found)?;
    let listed = std::process::Command::new(&nvidia_smi).arg("-L").output().map_err(|error| error.to_string())?;
    if !listed.status.success() || !String::from_utf8_lossy(&listed.stdout).contains("GPU ") {
        return Err("nvidia-smi lists no GPU on this machine, so there is no power limit to set.".to_string());
    }
    let user = std::env::var("USER").unwrap_or_default();
    let valid = user.chars().next().is_some_and(|first| first.is_ascii_lowercase() || first == '_')
        && user.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_' || c == '-');
    if !valid {
        return Err(format!("USER is {user:?}, which is not a plain account name, so no sudoers rule is written."));
    }
    let text = sudoers_text(&user, &nvidia_smi);
    println!("This writes {SUDOERS_PATH}:\n\n{text}");
    println!(
        "It lets {user} run only `{} -i <GPU index> -pl <watts>` as root without a password, so the Local controller can cap fleet GPU work and put the original limit back.",
        nvidia_smi.display()
    );
    println!("The rule never touches a file you can write: it is checked with `visudo -cf -`, piped into a root-owned");
    println!("{SUDOERS_STAGED} (sudo skips names with a dot), checked again there, then moved into place.");
    if !yes {
        println!("Nothing was changed. Run `local-studio fleet power-helper --yes` to install it.");
        return Ok(());
    }
    install_sudoers(&text, &nvidia_smi)
}

/// Sudo ignores files in /etc/sudoers.d whose names contain a dot, so a half-checked rule here is never live.
const SUDOERS_STAGED: &str = "/etc/sudoers.d/.local-studio-power.new";

fn run_with_stdin(program: &str, args: &[&str], input: &str) -> Result<std::process::Output, String> {
    use std::io::Write;
    let mut child = std::process::Command::new(program)
        .args(args)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .map_err(|error| format!("could not run {program}: {error}"))?;
    let mut stdin = child.stdin.take().ok_or("no stdin")?;
    stdin.write_all(input.as_bytes()).map_err(|error| error.to_string())?;
    drop(stdin);
    child.wait_with_output().map_err(|error| error.to_string())
}

fn said(output: &std::process::Output) -> String {
    format!("{}{}", String::from_utf8_lossy(&output.stdout).trim(), String::from_utf8_lossy(&output.stderr).trim())
}

fn install_sudoers(text: &str, nvidia_smi: &Path) -> Result<(), String> {
    let checked = run_with_stdin("visudo", &["-cf", "-"], text)
        .or_else(|_| run_with_stdin("/usr/sbin/visudo", &["-cf", "-"], text))
        .map_err(|_| "visudo is not installed, so the rule can't be checked before it is installed.".to_string())?;
    if !checked.status.success() {
        return Err(format!("visudo refused the rule, so nothing was installed: {}", said(&checked)));
    }
    let staged = run_with_stdin(
        "sudo",
        &["install", "-m", "0440", "-o", "root", "-g", "root", "/dev/stdin", SUDOERS_STAGED],
        text,
    )?;
    if !staged.status.success() {
        return Err(format!("sudo install failed, so {SUDOERS_PATH} was not written: {}", said(&staged)));
    }
    let sudo =
        |args: &[&str]| std::process::Command::new("sudo").args(args).output().map_err(|error| error.to_string());
    let rechecked = sudo(&["visudo", "-cf", SUDOERS_STAGED])?;
    if !rechecked.status.success() {
        let _ = sudo(&["rm", "-f", SUDOERS_STAGED]);
        return Err(format!("visudo refused the installed copy, so it was removed: {}", said(&rechecked)));
    }
    let moved = sudo(&["mv", "-f", SUDOERS_STAGED, SUDOERS_PATH])?;
    if !moved.status.success() {
        let _ = sudo(&["rm", "-f", SUDOERS_STAGED]);
        return Err(format!("could not move the rule into {SUDOERS_PATH}: {}", said(&moved)));
    }
    let allowed = std::process::Command::new("sudo")
        .args(["-n", "-l"])
        .arg(nvidia_smi)
        .args(["-i", "0", "-pl", "100"])
        .stdout(std::process::Stdio::null())
        .status()
        .map_err(|error| error.to_string())?;
    if !allowed.success() {
        return Err(format!("{SUDOERS_PATH} is installed, but sudo still asks for a password for nvidia-smi."));
    }
    println!("installed: the controller can now set the NVIDIA power limit for fleet work.");
    Ok(())
}
