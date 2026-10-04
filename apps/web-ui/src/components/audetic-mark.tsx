import { cn } from "@/lib/utils";
import icon from "../../../../assets/audetic_icon_light.svg";

type MarkState = "idle" | "recording" | "processing" | "review" | "offline" | "error";

const arcColor: Record<MarkState, string> = {
  idle: "bg-foreground/85",
  recording: "bg-[#b85346] dark:bg-[#e38a79]",
  processing: "bg-[#577c98] dark:bg-[#8cabc2]",
  review: "bg-[#a77b35] dark:bg-[#d7b373]",
  offline: "bg-foreground/85",
  error: "bg-destructive",
};

/** The small arc carries state; the original silhouette and outer arc stay still. */
export function AudeticMark({ state }: { state: MarkState }) {
  return <span
    aria-hidden="true"
    data-state={state}
    className={cn("relative block h-7 w-6 shrink-0", state === "offline" && "opacity-35")}
    style={{ maskImage: `url("${icon}")`, maskSize: "100% 100%", maskRepeat: "no-repeat" }}
  >
    <span className="absolute inset-0 bg-foreground/85" />
    {/* The SVG's two arcs are separated at x=385 in its 636-unit viewBox. */}
    <span className={cn("absolute inset-y-0 right-0 w-[40%]", arcColor[state])} />
  </span>;
}
