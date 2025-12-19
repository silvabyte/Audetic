# Inference Server Manager

A Bun-native HTTP service that manages a pool of WhisperServer workers for audio transcription. Powers the **voice.audetic.link** API.

## Features

- **Worker Pool Management**: Spawns and manages multiple WhisperServer instances
- **Load Balancing**: Round-robin distribution across healthy workers
- **Health Monitoring**: Automatic health checks every 5 seconds
- **Auto-Recovery**: Respawns failed workers with exponential backoff
- **Worker Recycling**: Rotates workers after configurable request threshold
- **OpenAPI Documentation**: Auto-generated API docs at `/openapi`

## Architecture

```
┌─────────────────────────────────────────────────────────────┐
│                  Inference Server Manager                    │
│                      (Elysia HTTP)                          │
├─────────────────────────────────────────────────────────────┤
│                                                             │
│  ┌─────────────┐  ┌─────────────┐  ┌─────────────┐         │
│  │  Worker 1   │  │  Worker 2   │  │  Worker 3   │  ...    │
│  │ :39000      │  │ :39001      │  │ :39002      │         │
│  │ (whisper)   │  │ (whisper)   │  │ (whisper)   │         │
│  └─────────────┘  └─────────────┘  └─────────────┘         │
│                                                             │
│  Health Checks (5s) │ Audit Sweep (30s) │ Auto-Rotation    │
└─────────────────────────────────────────────────────────────┘
```

## Quick Start

```bash
# Install dependencies
bun install

# Development (with hot reload)
bun run dev

# Production
bun run start
```

## Environment Variables

| Variable                     | Description                          | Default                 |
| ---------------------------- | ------------------------------------ | ----------------------- |
| `INFERENCE_SERVER_PORT`      | HTTP server port                     | `3141`                  |
| `TRANSCRIPTION_MANAGER_PORT` | (Legacy) HTTP server port            | `3141`                  |
| `WHISPER_SERVER_CMD`         | Path to whisper server executable    | Required                |
| `WHISPER_SERVER_CWD`         | Working directory for whisper server | Current dir             |
| `CORS_ORIGIN`                | Allowed CORS origin                  | `http://localhost:5173` |
| `LOG_LEVEL`                  | Pino log level                       | `info`                  |

## Configuration

Config file location: `~/.config/transcription_manager/settings.json5`

```json5
{
  // Number of worker processes
  workers: {
    poolSize: 3,
    rotateThreshold: 25, // Requests before worker rotation
    startingPort: 39000,
  },
  // WhisperServer configuration
  whisperServer: {
    cmd: "/path/to/whisper-server",
    cwd: "/path/to/whisper-project",
  },
  // Editor for config editing
  editor: "nvim",
}
```

## API Endpoints

### Health Check

```
GET /health
```

Returns service health and worker pool status.

### Submit Transcription

```
POST /api/v1/transcriptions
Content-Type: application/json

{
  "content": "<base64-encoded-audio>",
  "language": "en",
  "timestamps": true,
  "metadata": {}
}
```

### List Providers

```
GET /api/v1/providers
```

Returns available transcription providers and capabilities.

### Worker Pool Status

```
GET /api/v1/status
```

Returns detailed status of all workers in the pool.

## Deployment

This service powers `voice.audetic.link`. For deployment:

1. Ensure `WHISPER_SERVER_CMD` points to a valid whisper server binary
2. Set `CORS_ORIGIN` to your frontend domain
3. Configure worker pool size based on available resources
4. Use a process manager (systemd, pm2) for production

## Development

```bash
# Type check
bun run typecheck

# Format (from repo root)
bun run fmt

# Lint (from repo root)
bun run lint
```

## License

See repository root LICENSE file.
