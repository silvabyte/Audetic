import { toast } from "sonner";

export async function copyText(text: string, success = "Copied to clipboard"): Promise<boolean> {
  try { await navigator.clipboard.writeText(text); toast.success(success); return true; }
  catch { toast.error("Clipboard unavailable. Select the text and copy it manually."); return false; }
}
