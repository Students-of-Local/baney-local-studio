use std::path::Path;
use std::process::ExitCode;

use local_studio_core::plugins;

const USAGE: &str = "usage: local-studio plugin validate <dir>\n       local-studio plugin list";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    match args.as_slice() {
        ["plugin", "validate", dir] => validate(Path::new(dir)),
        ["plugin", "list"] => list(),
        _ => {
            eprintln!("{USAGE}");
            ExitCode::from(2)
        }
    }
}

fn validate(dir: &Path) -> ExitCode {
    match plugins::load(dir) {
        Ok(plugin) => {
            println!(
                "OK {} {} ({} skills, {} prompts, {} commands)",
                plugin.id,
                plugin.version,
                plugin.skills.len(),
                plugin.prompts.len(),
                plugin.commands.len()
            );
            ExitCode::SUCCESS
        }
        Err(errors) => {
            for error in errors {
                println!("error: {error}");
            }
            ExitCode::FAILURE
        }
    }
}

fn list() -> ExitCode {
    let Some(root) = plugins::plugins_dir(None) else {
        eprintln!("HOME is not set");
        return ExitCode::FAILURE;
    };
    println!("plugins dir: {}", root.display());
    let scan = plugins::scan(&root);
    for plugin in &scan.plugins {
        println!("{} {}: {}", plugin.id, plugin.version, plugin.description);
        for skill in &plugin.skills {
            println!("  skill {} ({})", skill.name, plugin.source());
        }
        for prompt in &plugin.prompts {
            println!("  prompt {} ({})", prompt.name, plugin.source());
        }
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
    if scan.failures.is_empty() {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}
