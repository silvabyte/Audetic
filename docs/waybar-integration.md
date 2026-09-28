# Waybar Integration

Use the same Audio Notes capture/status API as the CLI and native menu bar.
The status endpoint returns the capture lifecycle, not Waybar-specific JSON;
`jq` adapts it into Waybar's display format.

```jsonc
{
  "modules-center": ["custom/audetic", "clock"],
  "custom/audetic": {
    "exec": "curl -fsS http://127.0.0.1:3737/api/audio-notes/status | jq -c '{text: (if .phase == \"recording\" then \"󰻃\" else \"󰑊\" end), class: (\"audetic-\" + .phase), tooltip: (.last_error // .title // .phase)}'",
    "interval": 1,
    "return-type": "json",
    "on-click": "audetic notes toggle",
    "on-click-right": "audetic notes toggle --capture-source microphone_and_system",
    "tooltip": true
  }
}
```

Install `curl` and `jq`, and ensure `audetic` is on Waybar's PATH. Restart Waybar
after editing its config. Left-click starts microphone capture; right-click
starts microphone + system audio. Either stops the single active capture.
Automatic paste is off by default and clipboard copy is opt-in. Audio is retained.

If a capture was started with review enabled elsewhere, use the web UI or
`audetic notes confirm` / `audetic notes cancel` to resolve review.

Optional styling:

```css
#custom-audetic.audetic-recording { color: #ff6b6b; }
#custom-audetic.audetic-error { color: #ff1744; }
```

Troubleshooting: run `audetic notes status` to check daemon connectivity and
`audetic notes toggle` to test capture. Ensure `custom/audetic` is listed in
one of Waybar's modules arrays. Change the jq expression to customize icons
and tooltips; legacy `?style=waybar` is no longer an API contract.
