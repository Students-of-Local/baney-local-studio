---
name: inference-optimization
description: Tune a local LLM server for speed and GPU memory - benchmark decode tokens per second, prompt latency (TTFT) and VRAM, find the bottleneck, then adjust one engine flag per run (quantization, context length, KV cache, batching, prefix caching, speculative decoding, GPU offload, tensor parallelism) and keep the flags the benchmark proves. Use when a local model is slow or runs out of GPU memory, when choosing an engine (vLLM, SGLang, llama.cpp, Ollama, MLX) or a quant for a GPU, or when asked to tune, benchmark or speed up a model server.
license: AGPL-3.0
---

# Inference optimization: benchmark, tune one flag, verify

Tuning by feel makes a model server look quicker and answer worse. Every flag
here has a benchmark before it and after it, and quality is one of the numbers.

## Rules

- **Same bench every run:** same prompts, same output length, same machine.
  Run each measurement 3 times and take the median.
- **A shared GPU:** list what else runs on it (`nvidia-smi`) in the notes. If
  something else is using it, ask the user whether to pause it; if not, bench
  anyway, mark the numbers contended, and rerun when that load changes.
- **One flag per run.** Several flags at once can't tell you which one helped.
- **Quality counts:** after any change to quantization, KV cache precision or
  speculative decoding, rerun a fixed check of 10 to 20 prompts with known
  answers. A faster model that answers worse is a regression.
- **Keep the fast paths on:** CUDA graphs stay enabled (no `--enforce-eager`),
  and the server sets no output cap to make a run look quick. Both hide the
  real numbers.
- **Flags move between releases:** check each one against the installed
  engine's `--help` before you use it.
- **Other people's work:** never stop or restart an engine or GPU job you did
  not start without asking; other threads may be using it.

## 1. Baseline

Write `bench/<model>-<engine>.md` (a `/` in the model id becomes `-`) with the machine (GPU, VRAM, unified memory
on a Mac), engine and version, model and quant, the exact launch command, and:

| Number | How |
| --- | --- |
| Decode tokens/s, one request | stream exactly 256 tokens (`ignore_eos`, or llama.cpp `n_predict`); output tokens / streaming time after the opening chunk |
| Time to first token (TTFT), cold | a long prompt (8k tokens or the user's real size) whose start is unique each run (a nonce on line one), or the cache reset between runs (vLLM `/reset_prefix_cache`, SGLang `/flush_cache`); latency to the opening content chunk |
| TTFT, warm | the same prompt sent twice; the second latency. Judge prefix caching by this one |
| Throughput at real concurrency | 1 for one person, 4 to 16 for agents working in parallel |
| Memory | vLLM and SGLang reserve VRAM at startup, so record the KV cache size in tokens and max concurrency from the startup log, plus KV use from `/metrics`. llama.cpp, Ollama, MLX: peak from `nvidia-smi`, `rocm-smi` or Activity Monitor |

Token counts come from the response's `usage`, not a word count. Engines also
report live numbers at `/metrics` (`*:generation_tokens_total`,
`*:time_to_first_token_seconds`, KV cache use): vLLM always, SGLang with
`--enable-metrics`, llama.cpp server with `--metrics`. Their own bench tools work too: `vllm bench serve`,
`python -m sglang.bench_serving`, `llama-bench`.

Done when the notes hold every row and the command behind each.

## 2. Find the limit

| Symptom | Limit |
| --- | --- |
| Fails to load, out of memory at start | weights don't fit |
| Out of memory or requests preempted on long context or many requests; KV use near 100% | KV cache |
| One request decodes slowly, GPU busy | memory bandwidth |
| Long prompts wait before any output | prefill |
| One request is fine, parallel requests crawl | batching and scheduling |
| Slow while GPU utilization is low | CPU offload, wrong device, PCIe, or the client |

**Ceiling check:** one-request decode tokens/s is at most memory bandwidth
divided by the bytes read per token (active weights plus KV). An 8B model at
Q4 is about 4.5 GB, so a 1 TB/s card tops out near 220 tokens/s and real
runs reach 60 to 80% of that. For mixture-of-experts models count only active
parameters. Far below the ceiling means offload or misconfiguration, not a
need for a smaller quant.

Done when you can name one limit and the number that shows it.

## 3. Levers

Try the ones that match the limit, cheapest to most involved.

- **Everything on the GPU.** Partial CPU offload often costs more than a
  smaller quant. llama.cpp: `-ngl 999`. Ollama: `num_gpu`, checked with
  `ollama ps`. vLLM: no `--cpu-offload-gb`, and `--gpu-memory-utilization` high
  enough for the KV cache while leaving room for other processes on the card.
- **Context length to fit the need.** Set `--max-model-len` (vLLM),
  `--context-length` (SGLang), `-c` (llama.cpp) or `num_ctx` (Ollama) to what
  the work uses, not the model's maximum. It frees KV memory for batching.
- **Weight quantization.** GGUF `Q4_K_M` is the usual floor; `Q5_K_M` or `Q6_K`
  when memory allows. AWQ or GPTQ int4 for vLLM and SGLang. FP8 on cards with
  native FP8 (NVIDIA Ada, Hopper, Blackwell). Below Q4, check quality
  before keeping it.
- **Flash attention.** llama.cpp `-fa`, Ollama `OLLAMA_FLASH_ATTENTION=1`. Its
  own run, before KV quantization, which needs it.
- **KV cache precision.** vLLM `--kv-cache-dtype fp8`; llama.cpp
  `-ctk q8_0 -ctv q8_0`; Ollama `OLLAMA_KV_CACHE_TYPE=q8_0`. Roughly doubles
  the context that fits.
- **Prefix caching.** Agents resend a long system prompt every turn, so cache
  hits decide TTFT. vLLM `--enable-prefix-caching` (on by
  default in recent releases), SGLang's radix cache (on by default). Keep the
  prompt's start stable: no timestamps or random ids at the top.
- **Prefill.** Chunked prefill (vLLM `--enable-chunked-prefill`,
  `--max-num-batched-tokens`) keeps decoding smooth while long prompts load.
- **Batching.** vLLM `--max-num-seqs`; llama.cpp `-np <slots>` (the context is
  split across slots); Ollama `OLLAMA_NUM_PARALLEL`.
- **Speculative decoding.** A draft model, n-gram, EAGLE or MTP head. It speeds
  one request at low concurrency and slows busy servers. vLLM
  `--speculative-config`, SGLang `--speculative-algorithm`, llama.cpp
  `-md <draft.gguf>`. Watch the acceptance rate; under about 50% it rarely pays.
- **Several GPUs.** Tensor parallelism only with matched cards and a fast link.
  With mismatched cards, run a separate model on each.
- **Engine.** One person on a Mac: MLX or llama.cpp with Metal. One NVIDIA
  consumer card and GGUF weights: llama.cpp. Many parallel requests on NVIDIA:
  vLLM or SGLang. Ollama wraps llama.cpp and takes the settings above through
  its environment variables and `num_ctx`.

## 4. Judge each flag

After each flag, rerun step 1, and the quality check when the Rules call for it. Keep it when the target
number beats the run-to-run spread and quality holds; otherwise revert. Add a
line to the bench notes: change, before, after, kept or reverted.

Stop when the user's goal is met, or after three flags in a row that didn't
help.

## 5. Hand back

Give the final launch command, a before and after table, what you tried that
didn't help, and what was traded away (context length, a smaller quant).
