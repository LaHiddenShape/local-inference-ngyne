# local-inference-ngyne

A native Linux desktop app, written in Rust with egui, for chatting with local LLMs through Ollama.
It lets you browse, download and manage models from the GUI. Each model shows a link to its source page, and recommendations are based on your hardware (CPU, RAM, GPU VRAM, disk).

```sh
cargo run --release
```

## Persistence

Conversations, settings and the cached model lists are stored in `$XDG_DATA_HOME/local-inference-ngyne` (default `~/.local/share/local-inference-ngyne`). A directory left by the former name `local-inference-ui` is moved there on first launch.

Models themselves are managed by Ollama and stored in its own directory (`$OLLAMA_MODELS`, default `~/.ollama/models`).
