import type { AudeticBridge } from "./index";

declare global {
  interface Window {
    audetic: AudeticBridge;
  }
}

export {};
