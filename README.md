![local-inference-ngyne](assets/banner.png)

# local-inference-ngyne

A native Linux desktop app, written in Rust with egui, for chatting with local LLMs through Ollama.
It lets you browse, download and manage models from the GUI. Each model shows a link to its source page, and recommendations are based on your hardware (CPU, RAM, GPU VRAM, disk).

```sh
cargo run --release
```

To add it to the desktop's application list, with its icon:

```sh
cargo build --release
install -Dm755 target/release/local-inference-ngyne ~/.local/bin/local-inference-ngyne
install -Dm644 assets/icon.png ~/.local/share/icons/hicolor/256x256/apps/local-inference-ngyne.png
install -Dm644 assets/local-inference-ngyne.desktop ~/.local/share/applications/local-inference-ngyne.desktop
```

## Persistence

Conversations, settings and the cached model lists are stored in `$XDG_DATA_HOME/local-inference-ngyne` (default `~/.local/share/local-inference-ngyne`). A directory left by the former name `local-inference-ui` is moved there on first launch.

Each conversation is one JSON file in `conversations/`, saved in the background with an atomic, fsynced write. A file that cannot be read is reported at startup and left untouched, never overwritten. The single `conversations.json` of earlier versions is split into these files on first launch.

Models themselves are managed by Ollama and stored in its own directory (`$OLLAMA_MODELS`, default `~/.ollama/models`).
