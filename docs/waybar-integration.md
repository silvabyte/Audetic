# Waybar Integration

Add Audetic status indicators to your Waybar.

## Setup

### 1. Add Audetic Module to Waybar Config

Add the module to your modules list and configuration:

```jsonc
{
  "modules-center": ["custom/audetic", "clock"], // Add to any module list
  
  "custom/audetic": {
    "exec": "curl -s 'http://127.0.0.1:3737/status?style=waybar'",
    "interval": 1,
    "return-type": "json", 
    "on-click": "curl -X POST http://127.0.0.1:3737/toggle",
    "tooltip": true
  }
}
```

### 2. Restart Waybar

```bash
pkill waybar && waybar
```

## API Response

The endpoint returns JSON with different icons for each state:

- **Idle**: `󰑊` (circle with dot)
- **Recording**: `󰻃` (record button)  

Example response:
```json
{
  "text": "󰑊",
  "class": "audetic-idle", 
  "tooltip": "Press Super+R to record"
}
```

## Customization

Customize icons and tooltips in your Audetic config (`~/.config/audetic/config.toml`):

```toml
[ui.waybar]
idle_text = "󰍬"                # Use microphone icon
recording_text = "●"            # Use simple filled circle  
idle_tooltip = "Click to record"
recording_tooltip = "Recording..."
```

CSS styling (optional):
```css
#custom-audetic.audetic-recording {
  color: #ff6b6b;
  animation: pulse 2s infinite;
}
```

## Troubleshooting

**Module not appearing**: Ensure `"custom/audetic"` is added to a module list (modules-left, modules-center, or modules-right).

**Shows "N/A" or error**: Check Audetic is running: `curl http://127.0.0.1:3737/status`

**Click not working**: Test the command manually: `curl -X POST http://127.0.0.1:3737/toggle`

