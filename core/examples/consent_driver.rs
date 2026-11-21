use std::collections::BTreeMap;
use std::sync::{Arc, Condvar, Mutex, PoisonError};
use std::time::{Duration, Instant};

use anyhow::{bail, Context};
use local_studio_core::{ConsentAnswer, ConsentNeed, ConsentRequest, Studio, StudioConfig, Update, Waker};

#[derive(Default)]
struct Signal {
    pending: Mutex<bool>,
    ready: Condvar,
}

impl Waker for Signal {
    fn wake(&self) {
        *self.pending.lock().unwrap_or_else(PoisonError::into_inner) = true;
        self.ready.notify_one();
    }
}

struct Driver {
    studio: Arc<Studio>,
    signal: Arc<Signal>,
    open: Vec<ConsentRequest>,
    outcomes: BTreeMap<String, Result<String, String>>,
}

impl Driver {
    fn pump(&mut self, done: impl Fn(&Self) -> bool, limit: Duration) -> bool {
        let deadline = Instant::now() + limit;
        loop {
            for update in self.studio.tick() {
                match update {
                    Update::Consents { consents } => self.open = consents,
                    Update::RequestDone { request, message } => drop(self.outcomes.insert(request, Ok(message))),
                    Update::RequestFailed { request, message } => drop(self.outcomes.insert(request, Err(message))),
                    _ => {}
                }
            }
            if done(self) {
                return true;
            }
            if Instant::now() >= deadline {
                return false;
            }
            let pending = self.signal.pending.lock().unwrap_or_else(PoisonError::into_inner);
            let (mut pending, _) = self
                .signal
                .ready
                .wait_timeout_while(pending, Duration::from_millis(200), |pending| !*pending)
                .unwrap_or_else(PoisonError::into_inner);
            *pending = false;
        }
    }

    fn sheet(&self, request: &str) -> Option<&ConsentRequest> {
        self.open.iter().find(|sheet| sheet.request == request)
    }
}

fn print_sheet(sheet: &ConsentRequest) {
    let rows: Vec<String> = sheet
        .rows
        .iter()
        .map(|row| format!("{}:{}", row.id, if row.need == ConsentNeed::Sensitive { "sensitive" } else { "needed" }))
        .collect();
    println!(
        "SHEET {} {} from={} source={} pin={} files={} findings={} rows={}",
        sheet.name,
        sheet.version,
        sheet.update_from.as_deref().unwrap_or("-"),
        sheet.source,
        sheet.pin.as_deref().unwrap_or("-"),
        sheet.files,
        sheet.findings.len(),
        rows.join(",")
    );
    for finding in &sheet.findings {
        println!("FINDING {finding}");
    }
    for row in &sheet.rows {
        println!("ROW {} | {} | {} | {}", row.id, row.sentence, row.scope, row.gates.as_deref().unwrap_or("-"));
    }
}

fn parse(answer: &str) -> anyhow::Result<ConsentAnswer> {
    Ok(match answer.split_once(':') {
        _ if answer == "cancel" => ConsentAnswer::Cancel,
        _ if answer == "install" => ConsentAnswer::Install { sensitive: Vec::new() },
        Some(("install", on)) => ConsentAnswer::Install { sensitive: on.split(',').map(str::to_string).collect() },
        _ => bail!("unknown answer {answer}"),
    })
}

fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let [data, socket, source, answers @ ..] = args.as_slice() else {
        bail!("usage: consent_driver <data dir> <socket> <plugin source | scan> <cancel|install[:sensitive,...] | scan: request=answer ...>");
    };
    let scan = source == "scan";
    let answers: Vec<(String, String)> = if scan {
        answers
            .iter()
            .map(|pair| pair.split_once('=').map(|(request, answer)| (request.to_string(), answer.to_string())))
            .collect::<Option<_>>()
            .context("scan answers are request=answer")?
    } else {
        let [answer] = answers else { bail!("one answer for an install") };
        vec![("consent-driver".to_string(), answer.clone())]
    };
    let config = StudioConfig {
        socket_path: Some(socket.clone()),
        session_dir: None,
        data_dir: Some(data.clone()),
        launch: None,
        controller_main: None,
        take_over_serve: false,
    };
    let signal = Arc::new(Signal::default());
    let studio = Studio::new(config, Arc::clone(&signal) as Arc<dyn Waker>).context("start Studio")?;
    let mut driver = Driver { studio: Arc::clone(&studio), signal, open: Vec::new(), outcomes: BTreeMap::new() };
    if !scan {
        studio.install_plugin(answers[0].0.clone(), source.clone());
    }
    let shown = |driver: &Driver| {
        answers.iter().all(|(request, _)| driver.sheet(request).is_some() || driver.outcomes.contains_key(request))
    };
    if !driver.pump(shown, Duration::from_mins(2)) {
        bail!("no sheet and no answer within 2 minutes");
    }
    let open: Vec<&(String, String)> =
        answers.iter().filter(|(request, _)| !driver.outcomes.contains_key(request)).collect();
    for (request, _) in &open {
        if scan {
            println!("OPEN {request}");
        }
        if let Some(sheet) = driver.sheet(request) {
            print_sheet(sheet);
        }
    }
    if !open.is_empty() {
        // A subscriber that joins after the sheet was raised, like the Remote hub after serve.json
        // appears, must get it in its first batch.
        let late = studio.subscribe(Arc::new(Signal::default()) as Arc<dyn Waker>);
        let seen: Vec<String> = late
            .tick()
            .into_iter()
            .filter_map(|update| match update {
                Update::Consents { consents } => Some(consents),
                _ => None,
            })
            .flatten()
            .map(|sheet| sheet.request)
            .collect();
        let replayed = open.iter().all(|(request, _)| seen.contains(request));
        println!("LATE {}", if replayed { "sees the sheet" } else { "misses the sheet" });
    }
    if std::env::var_os("CONSENT_DRIVER_AGAIN").is_some() && !scan {
        let request = answers[0].0.clone();
        studio.install_plugin(request.clone(), source.clone());
        let answered = driver.pump(|driver| driver.outcomes.contains_key(&request), Duration::from_secs(20));
        match driver.outcomes.remove(&request) {
            Some(Err(message)) if answered => println!("AGAIN refused: {message}"),
            Some(Ok(message)) => println!("AGAIN done: {message}"),
            _ => println!("AGAIN no answer within 20 s"),
        }
    }
    if let Some(hook) = std::env::var_os("CONSENT_DRIVER_HOOK") {
        let status = std::process::Command::new("sh").arg("-c").arg(hook).status().context("run the hook")?;
        println!("HOOK {status}");
    }
    for (request, answer) in &open {
        studio.answer_consent(request.clone(), parse(answer)?);
    }
    let answered = |driver: &Driver| answers.iter().all(|(request, _)| driver.outcomes.contains_key(request));
    if !driver.pump(answered, Duration::from_mins(2)) {
        bail!("no answer within 2 minutes");
    }
    let mut code = 0;
    for (request, _) in &answers {
        let label = if scan { format!(" {request}:") } else { String::new() };
        match &driver.outcomes[request] {
            Ok(message) => println!("DONE{label} {message}"),
            Err(message) => {
                println!("FAILED{label} {message}");
                code = 1;
            }
        }
    }
    let after: Vec<&str> = driver.open.iter().map(|sheet| sheet.request.as_str()).collect();
    println!("OPEN-AFTER {}", if after.is_empty() { "-".to_string() } else { after.join(",") });
    studio.shutdown();
    std::process::exit(code)
}
