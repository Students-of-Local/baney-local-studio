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

enum Seen {
    Sheet(Box<ConsentRequest>),
    Done(String),
    Failed(String),
}

fn next(studio: &Studio, signal: &Signal, request: &str) -> anyhow::Result<Seen> {
    let deadline = Instant::now() + Duration::from_mins(2);
    while Instant::now() < deadline {
        for update in studio.tick() {
            match update {
                Update::Consents { consents } => {
                    if let Some(sheet) = consents.into_iter().find(|sheet| sheet.request == request) {
                        return Ok(Seen::Sheet(Box::new(sheet)));
                    }
                }
                Update::RequestDone { request: done, message } if done == request => return Ok(Seen::Done(message)),
                Update::RequestFailed { request: done, message } if done == request => {
                    return Ok(Seen::Failed(message));
                }
                _ => {}
            }
        }
        let pending = signal.pending.lock().unwrap_or_else(PoisonError::into_inner);
        let (mut pending, _) = signal
            .ready
            .wait_timeout_while(pending, Duration::from_millis(200), |pending| !*pending)
            .unwrap_or_else(PoisonError::into_inner);
        *pending = false;
    }
    bail!("no sheet and no answer for {request} within 2 minutes")
}

fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let [data, socket, source, answer] = args.as_slice() else {
        bail!("usage: consent_driver <data dir> <socket> <plugin source> <cancel|install[:sensitive,...]>");
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
    let request = "consent-driver".to_string();
    studio.install_plugin(request.clone(), source.clone());
    let outcome = match next(&studio, &signal, &request)? {
        Seen::Sheet(sheet) => {
            let rows: Vec<String> = sheet
                .rows
                .iter()
                .map(|row| {
                    format!("{}:{}", row.id, if row.need == ConsentNeed::Sensitive { "sensitive" } else { "needed" })
                })
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
            for row in &sheet.rows {
                println!("ROW {} | {} | {} | {}", row.id, row.sentence, row.scope, row.gates.as_deref().unwrap_or("-"));
            }
            // A subscriber that joins after the sheet was raised, like the Remote hub after serve.json
            // appears, must get it in its first batch.
            let late = studio.subscribe(Arc::new(Signal::default()) as Arc<dyn Waker>);
            let replayed = late.tick().into_iter().any(|update| {
                matches!(update, Update::Consents { consents } if consents.iter().any(|sheet| sheet.request == request))
            });
            println!("LATE {}", if replayed { "sees the sheet" } else { "misses the sheet" });
            let answer = match answer.split_once(':') {
                _ if answer == "cancel" => ConsentAnswer::Cancel,
                Some(("install", on)) => {
                    ConsentAnswer::Install { sensitive: on.split(',').map(str::to_string).collect() }
                }
                _ if answer == "install" => ConsentAnswer::Install { sensitive: Vec::new() },
                _ => bail!("unknown answer {answer}"),
            };
            studio.answer_consent(request.clone(), answer);
            next(&studio, &signal, &request)?
        }
        seen => seen,
    };
    let code = match outcome {
        Seen::Done(message) => {
            println!("DONE {message}");
            0
        }
        Seen::Failed(message) => {
            println!("FAILED {message}");
            1
        }
        Seen::Sheet(_) => {
            println!("FAILED the sheet came back after its answer");
            1
        }
    };
    studio.shutdown();
    std::process::exit(code)
}
