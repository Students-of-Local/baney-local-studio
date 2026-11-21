# End-to-end gate

`scripts/e2e.sh` runs every core example (`core/examples/*_smoke.rs`) on a clean checkout of one commit and, with `--post`, posts the `local-studio/e2e` commit status that the nightly reads (docs/channels.md, Promotion rule). It runs on abbey1 once a day against abbey1's own Ollama.

## The model

The examples take the model from the environment and drive nothing else:

| Variable | Default | Meaning |
| --- | --- | --- |
| `LOCAL_STUDIO_SMOKE_MODEL` | required | `<provider>/<model>`, for example `ollama/fv-smoke-lfm3-1.7b` |
| `LOCAL_STUDIO_SMOKE_BASE_URL` | `http://127.0.0.1:11434/v1` | the OpenAI-compatible endpoint that serves it |
| `LOCAL_STUDIO_SMOKE_CONTEXT` | `32768` | the `contextWindow` written to the scratch catalog |
| `LOCAL_STUDIO_SMOKE_IMAGE_MODEL` | the main model | a vision model on the same provider for the one image turn, for example `ollama/qwen3-vl:4b` |

Each example writes its own scratch agent dir: a `models.json` with only that provider (`api` `openai-completions`, `apiKey` `none`, the context window above, and a nominal price of $1 per million tokens so the usage report's cost path runs), a `settings.json` naming it as the default, and an `auth.json` of `{}`. Nothing is linked to a real credential, and `local_studio_core::check_scratch_agent` refuses the dir if `auth.json` holds a key. When `LOCAL_STUDIO_SMOKE_IMAGE_MODEL` names another model, the catalog lists it too, with image input and thinking off (`reasoning_effort: "none"`), and `smoke` starts a fresh thread on it for the image turn (a short prompt, since the vision model runs mostly on the CPU) and requires the reply to name the image's color. Without it the main model is listed with image input and answers that turn itself. `plugin_smoke` gets the same model as a keyless `local-fleet` provider (override it with `PLUGIN_SMOKE_MODEL`).

No Anthropic model runs here: Eukhe's Anthropic subscription sign-in is ruled out (Donovan, 2026-11-17) and API keys are paid billing. Claude and Codex coverage comes from `harness_claude_smoke` and `harness_codex_smoke`, which run the official `claude` and `codex` CLIs on their own sign-in.

The gate model is `fv-smoke-lfm3-1.7b` (LFM3 1.7B Q4_K_M) because it is the one model on abbey1 that fits its 4 GB GPU with a 32768 context and still calls tools. Measured on abbey1 on 2026-11-21 with an idle GPU and a 7227-token prompt: 551 tok/s prompt, 87.5 tok/s generation, all of it in VRAM. Qwen3 4B Instruct at the same context spills to the CPU and generates 2.9 tok/s. abbey1's Ollama must serve a context of at least 32768 (`OLLAMA_CONTEXT_LENGTH=32768` in `ollama.service.d/context.conf`); at Ollama's 4096 default Eukhe's system prompt alone overflows. `fv-smoke-lfm3-1.7b` cannot see images, so the gate's image turn runs on `qwen3-vl:4b`, the one vision model on abbey1 that Ollama lets call tools (Eukhe always sends tools, and Ollama refuses them for LFM2-VL and Gemma 3). It runs mostly on the CPU there: about 100 s for a short turn with thinking off, against more than 300 s with Ollama's default thinking. Ollama serves one request at a time, so the gate runs one example at a time.

The time limit for each model step is 600 s, sized for a slow local model. The bounds that describe app behavior (Stop within 20 s, the crashed worker agreeing within 45 s, and the rest) are unchanged.

## The GPU window

abbey1's GPU is shared through `~/.local/bin/ollama-window`. After the build, `scripts/e2e.sh` waits for the window (`ollama-window wait`, giving up after `E2E_WINDOW_WAIT_MINUTES`, default 180), claims it for `E2E_WINDOW_MINUTES` (default 60) under `E2E_WINDOW_WHO` (default `local-studio-e2e`), claims again before each example so the claim never lapses mid-run, and releases it on exit. A host without `ollama-window` runs without one.

## Run it by hand

On abbey1, in a clean clone that nobody else uses:

```sh
export PATH=$HOME/.cargo/bin:$HOME/.local/bin:$HOME/.local/opt/nu-0.117.0:$HOME/.local/opt/zig-0.15.2:$HOME/.cache/tychicus-claude/bunbin:$PATH
export LOCAL_STUDIO_SMOKE_MODEL=ollama/fv-smoke-lfm3-1.7b LOCAL_STUDIO_SMOKE_IMAGE_MODEL=ollama/qwen3-vl:4b
scripts/e2e.sh          # no status
scripts/e2e.sh --post   # posts local-studio/e2e
```

It ends with each example's time and the total. The status description names the model, for example `local model ollama/fv-smoke-lfm3-1.7b: all 11 core e2e checks passed on abbey1`.

## The daily timer on abbey1

`scripts/e2e-abbey1/` holds a systemd user service and timer. The timer fires at 02:00 UTC, before the 07:00 UTC nightly; the service fetches `main` into its own clone, checks out its head and runs `scripts/e2e.sh --post`. Install it on abbey1, not chapel1:

```sh
ssh abbey1
git clone https://github.com/baney75/local-studio ~/Projects/_work/local-studio-e2e
cd ~/Projects/_work/local-studio-e2e/scripts/e2e-abbey1
install -m 0644 local-studio-e2e.service local-studio-e2e.timer ~/.config/systemd/user/
install -m 0600 local-studio-e2e.env ~/.config/local-studio-e2e.env
systemctl --user daemon-reload
systemctl --user enable --now local-studio-e2e.timer
systemctl --user list-timers local-studio-e2e.timer
```

`local-studio-e2e.env` sets the model and the `PATH` (cargo, nu, zig 0.15.2, bun, `gh` signed in as baney75); edit it when a tool moves. Linger must be on (`loginctl show-user "$USER" -p Linger` shows `yes`) so the timer runs with nobody logged in. Run it now with `systemctl --user start local-studio-e2e.service` and read it with `journalctl --user -u local-studio-e2e`. Nothing else may use that clone: the gate refuses a dirty checkout. When the unit files change on `main`, copy them again and run `systemctl --user daemon-reload`.
