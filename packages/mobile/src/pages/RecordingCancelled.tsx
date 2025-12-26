import { Icon } from "@iconify/react";

export function RecordingCancelled() {
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
					<div className="relative flex items-center justify-center size-72">
						<div className="absolute inset-0 rounded-full border border-border/30" />
						<div className="absolute inset-4 rounded-full border border-border/20" />
						<div className="relative z-10 size-40 rounded-full bg-card border border-border flex items-center justify-center shadow-lg">
							<div className="absolute inset-1 rounded-full border border-white/5" />
							<div className="size-20 rounded-full bg-primary/10 border border-primary/20 flex items-center justify-center">
								<Icon icon="solar:close-circle-bold" className="size-12 text-primary" />
							</div>
						</div>
					</div>
					<div className="flex flex-col items-center gap-3 text-center">
						<h2 className="text-xl font-medium text-foreground font-heading">
							Recording Cancelled
						</h2>
						<p className="text-sm text-muted-foreground max-w-xs">
							Your recording has been cancelled and will not be saved
						</p>
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
