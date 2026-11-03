# Windows Stats for AI apps (MCP)

`WindowsStats.exe --mcp` is a read-only [Model Context Protocol](https://modelcontextprotocol.io) server. An AI app that supports MCP starts it, talks to it through pipes (stdin/stdout) and can then answer questions such as *"How much video memory is free?"*, *"What is slowing my computer down?"* or *"How hot did the GPU get during my last Ollama run?"*. It works with cloud models and with local ones (Ollama, llama.cpp, LM Studio's own models) as long as the chat app speaks MCP and the model can call tools.

It opens no network port, changes nothing and runs only while the AI app keeps it open. It measures only when asked: after a question it samples once a second for two minutes (for `recent_history`), then waits idle.

## Tools

| Tool | What it returns |
|---|---|
| `system_overview` | CPU (total, per logical processor, clock), memory, GPU load and video memory, disk activity and free space per drive, network throughput, battery, the temperatures Windows reports. |
| `recent_history` | CPU, memory, GPU, disk and network once a second for the last `seconds` (5–120, default 60) since the first question. |
| `top_processes` | The programs using the most CPU or memory (`sort_by`: `cpu` / `memory`, `limit` 1–15). **Returns program names.** |
| `ai_sessions` | Local AI sessions Windows Stats followed (runtime, model, duration, busy time, peak and average CPU and GPU, peak video memory, memory and temperatures), including one running now. Empty unless session following is on in Settings → AI. |

Network addresses, adapter and Wi-Fi names, drive labels and file paths are never returned.

## Set it up

1. Install Windows Stats (0.1.7 or newer) with `Kur.cmd`.
2. Settings → **AI** → **Copy the MCP setup**. The clipboard then holds:

   ```json
   {
     "mcpServers": {
       "windows-stats": {
         "command": "C:\\Users\\<you>\\AppData\\Local\\Programs\\Windows Stats\\WindowsStats.exe",
         "args": ["--mcp"]
       }
     }
   }
   ```

3. Paste it into the AI app's MCP settings and restart the app if it asks for that:
   - **Claude Desktop:** Settings → Developer → Edit Config (`claude_desktop_config.json`); merge the `windows-stats` entry into `mcpServers`.
   - **LM Studio:** its `mcp.json` (in recent versions under Program → Install → Edit mcp.json) uses the same `mcpServers` format.
   - **VS Code:** `.vscode/mcp.json` calls the list `servers`:
     ```json
     { "servers": { "windows-stats": { "type": "stdio", "command": "C:\\...\\WindowsStats.exe", "args": ["--mcp"] } } }
     ```
   - Other MCP clients: any client that can start a stdio server with a command and arguments.

### With Ollama

Ollama runs the model; MCP tools come from the chat app in front of it. Use a chat app that supports MCP with Ollama models, and a model that supports tool calling (for example Llama 3.1 or newer, Qwen 2.5 or newer). Very small models may call tools poorly.

## Privacy

- Everything is read on this computer. With a **local** model nothing leaves it.
- With a **cloud** model, the AI app sends the tool results to that model's service. That includes program names when the model calls `top_processes`; its description says so, and most AI apps ask before running a tool.
- `ai_sessions` reads only what Windows Stats saved locally (`ai-sessions.jsonl`, `ai-session-live.json` in `%APPDATA%\WindowsStats`).

## Try it without an AI app

Put the requests in a file, one JSON object per line (`requests.jsonl`):

```json
{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18","capabilities":{}}}
{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"system_overview","arguments":{}}}
```

and pipe it in from a Command Prompt (PowerShell does not pass pipes to a windowed program reliably):

```bat
type requests.jsonl | "%LOCALAPPDATA%\Programs\Windows Stats\WindowsStats.exe" --mcp
```

Each answer is one JSON line. The server supports MCP protocol revisions 2025-06-18, 2025-03-26 and 2024-11-05.
