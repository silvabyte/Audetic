import { makeAutoObservable, runInAction } from "mobx";
import { daemon } from "@/api/client";
import type { components } from "@/api/schema";
import type { RootStore } from "./root-store";

export type SetupAssessment = components["schemas"]["SetupAssessment"];
export type SetupCapability = components["schemas"]["SetupCapability"];
export type SetupCapabilityId = components["schemas"]["SetupCapabilityId"];
export type SetupState = components["schemas"]["SetupState"];

type LoadState = "idle" | "loading" | "loaded" | "error";

/** Server-authored setup truth for the Setup Center. */
export class SetupStore {
  assessment: SetupAssessment | null = null;
  state: LoadState = "idle";
  error: string | null = null;

  private root: RootStore;

  constructor(root: RootStore) {
    this.root = root;
    makeAutoObservable<this, "root">(this, { root: false });
  }

  get loading(): boolean {
    return this.state === "loading";
  }

  get dictationReady(): boolean {
    return isReady(this.assessment?.workflows.dictation);
  }

  get meetingsReady(): boolean {
    return isReady(this.assessment?.workflows.meetings);
  }

  capability(id: SetupCapabilityId): SetupCapability | undefined {
    return this.assessment?.capabilities.find((capability) => capability.id === id);
  }

  get nextRequiredAction(): SetupCapability | undefined {
    return this.assessment?.capabilities.find(
      (capability) =>
        (capability.required_for_dictation || capability.required_for_meetings) &&
        !isReady(capability.state),
    );
  }

  async load(): Promise<void> {
    runInAction(() => {
      this.state = "loading";
      this.error = null;
    });
    try {
      const { data, error } = await daemon.GET("/setup");
      if (error || !data) throw new Error(formatError(error ?? "empty response"));
      runInAction(() => {
        this.assessment = data;
        this.state = "loaded";
      });
    } catch (error) {
      runInAction(() => {
        this.state = "error";
        this.error = error instanceof Error ? error.message : String(error);
      });
    }
  }

  async recheck(): Promise<void> {
    await Promise.all([this.load(), this.root.onboarding.check()]);
  }
}

export function isReady(state: SetupState | undefined): boolean {
  return state === "ready" || state === "not_applicable";
}

function formatError(error: unknown): string {
  if (typeof error === "string") return error;
  if (error && typeof error === "object" && "message" in error) {
    return String((error as { message: unknown }).message);
  }
  try {
    return JSON.stringify(error);
  } catch {
    return String(error);
  }
}
