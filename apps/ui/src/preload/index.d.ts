import type {
  AudeticBridge,
  OnboardingProgress,
  OnboardingResult,
  OnboardingState,
  ThemeMode,
} from "./index";

declare global {
  interface Window {
    audetic: AudeticBridge;
  }
}

// Re-export types so renderer code can import them from "@/preload" without
// also pulling in the preload module itself (which uses Electron node APIs).
export type {
  AudeticBridge,
  OnboardingProgress,
  OnboardingResult,
  OnboardingState,
  ThemeMode,
};
