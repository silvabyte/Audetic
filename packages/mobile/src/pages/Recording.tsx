import { Icon } from "@iconify/react";

export function Recording() {
	return (
		<div className="flex flex-col h-full bg-background text-foreground font-sans selection:bg-primary/20 overflow-hidden relative">
			<header className="flex items-center justify-between px-6 pt-12 pb-4 z-20">
				<div className="flex items-center gap-2">
					<span className="text-xs font-medium tracking-widest text-muted-foreground uppercase">
						Voice Recorder
					</span>
				</div>
				<div className="flex items-center gap-4">
					<div className="flex items-center gap-1.5 px-3 py-1.5 rounded-full bg-card/50 border border-border/50">
						<Icon
							icon="solar:battery-charge-minimalistic-linear"
							className="size-4 text-muted-foreground"
						/>
						<span className="text-xs text-muted-foreground font-medium">100%</span>
					</div>
				</div>
			</header>
			<main className="flex-1 flex flex-col items-center justify-center z-10 relative px-6">
				<div className="flex flex-col items-center gap-8">
					<div className="relative flex items-center justify-center size-80">
						<div className="absolute inset-0 rounded-full border border-primary/20 animate-pulse" />
						<div className="absolute inset-4 rounded-full border border-primary/10" />
						<div className="absolute inset-8 rounded-full bg-primary/5" />
						<button className="relative z-10 size-48 rounded-full bg-primary border-2 border-primary flex items-center justify-center shadow-2xl shadow-primary/20 active:scale-95 transition-all duration-300 group">
							<div className="absolute inset-1 rounded-full border border-white/10" />
							<div className="absolute inset-0 rounded-full bg-gradient-to-b from-white/10 to-transparent" />
							<Icon
								icon="solar:microphone-3-bold"
								className="size-16 text-primary-foreground group-active:scale-95 transition-transform"
							/>
						</button>
						<div
							style={{ animationDuration: "2s" }}
							className="absolute -inset-2 rounded-full border border-primary/30 animate-ping"
						/>
					</div>
					<div className="flex flex-col items-center gap-3 text-center">
						<div className="flex items-center gap-2 px-4 py-2 rounded-full bg-primary/10 border border-primary/20">
							<div className="size-2 rounded-full bg-primary animate-pulse" />
							<h2 className="text-lg font-medium text-foreground font-heading">Listening</h2>
						</div>
						<p className="text-sm text-muted-foreground max-w-xs">
							Speak clearly into the microphone
						</p>
					</div>
					<div class="flex flex-col items-center gap-8 mt-12 w-full max-w-xs">
						<div class="flex items-center justify-center">
							<div class="flex flex-col items-center gap-2">
								<span class="text-2xl font-medium text-foreground font-heading">00:23</span>
								<span class="text-xs text-muted-foreground uppercase tracking-wide">Duration</span>
							</div>
						</div>
						<div class="flex items-center justify-center gap-6 w-full">
							<button class="size-14 rounded-full bg-card border border-border flex items-center justify-center shadow-lg active:scale-95 transition-all duration-200">
								<Icon icon="solar:pause-bold" class="size-6 text-foreground" />
							</button>
							<button class="size-16 rounded-full bg-destructive border border-destructive flex items-center justify-center shadow-lg active:scale-95 transition-all duration-200">
								<Icon icon="solar:stop-bold" class="size-7 text-primary-foreground" />
							</button>
						</div>
					</div>
				</div>
			</main>
			<nav className="border-t border-border bg-background/95 backdrop-blur-md pb-safe pt-2 z-30">
				<div className="flex items-center justify-around h-16">
					<div className="flex flex-col items-center gap-1 w-20 cursor-pointer">
						<div className="relative">
							<Icon icon="solar:home-2-bold" className="size-6 text-foreground" />
							<div className="absolute -bottom-2 left-1/2 -translate-x-1/2 w-1 h-1 rounded-full bg-foreground" />
						</div>
						<span className="text-[10px] font-medium text-foreground">Home</span>
					</div>
					<div className="flex flex-col items-center gap-1 w-20 cursor-pointer opacity-60 hover:opacity-100 transition-opacity">
						<Icon icon="solar:history-bold" className="size-6 text-muted-foreground" />
						<span className="text-[10px] font-medium text-muted-foreground">History</span>
					</div>
					<div className="flex flex-col items-center gap-1 w-20 cursor-pointer opacity-60 hover:opacity-100 transition-opacity">
						<Icon icon="solar:settings-bold" className="size-6 text-muted-foreground" />
						<span className="text-[10px] font-medium text-muted-foreground">Settings</span>
					</div>
				</div>
			</nav>
		</div>
	);
}
