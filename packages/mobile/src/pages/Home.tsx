import { Icon } from "@iconify/react";

export function Home() {
	return (
		<div
			eid="e1"
			className="flex flex-col h-full bg-background text-foreground font-sans selection:bg-primary/20 overflow-hidden relative"
		>
			<header eid="e3" className="flex items-center justify-between px-6 pt-12 pb-4 z-20">
				<div eid="e4" className="flex items-center gap-2">
					<span
						eid="e6"
						className="text-xs font-medium tracking-widest text-muted-foreground uppercase"
					>
						Voice Recorder
					</span>
				</div>
				<div eid="e7" className="flex items-center gap-4">
					<div
						eid="e8"
						className="flex items-center gap-1.5 px-3 py-1.5 rounded-full bg-card/50 border border-border/50"
					>
						<Icon
							eid="e9"
							icon="solar:battery-charge-minimalistic-linear"
							className="size-4 text-muted-foreground"
						/>
						<span eid="e10" className="text-xs text-muted-foreground font-medium">
							100%
						</span>
					</div>
				</div>
			</header>
			<main
				eid="e11"
				className="flex-1 flex flex-col items-center justify-center z-10 relative px-6"
			>
				<div eid="e12" className="flex flex-col items-center gap-8">
					<div eid="e15" className="relative flex items-center justify-center size-72">
						<div eid="e16" className="absolute inset-0 rounded-full border border-border/30" />
						<div eid="e17" className="absolute inset-4 rounded-full border border-border/20" />
						<button
							eid="e19"
							className="relative z-10 size-40 rounded-full bg-card border border-border flex items-center justify-center shadow-lg active:scale-95 transition-all duration-300 group"
						>
							<div eid="e20" className="absolute inset-1 rounded-full border border-white/5" />
							<Icon
								eid="e21"
								icon="solar:microphone-3-bold"
								className="size-14 text-foreground/80 group-active:text-foreground transition-colors"
							/>
						</button>
					</div>
					<div eid="e24" className="flex flex-col items-center gap-3 text-center">
						<h2 eid="e25" className="text-xl font-medium text-foreground font-heading">
							Ready to Record
						</h2>
						<p eid="e26" className="text-sm text-muted-foreground max-w-xs">
							Tap the microphone to start a new voice recording
						</p>
					</div>
				</div>
			</main>
			<nav
				eid="e43"
				className="border-t border-border bg-background/95 backdrop-blur-md pb-safe pt-2 z-30"
			>
				<div eid="e44" className="flex items-center justify-around h-16">
					<div eid="e45" className="flex flex-col items-center gap-1 w-20 cursor-pointer">
						<div eid="e46" className="relative">
							<Icon eid="e47" icon="solar:home-2-bold" className="size-6 text-foreground" />
							<div
								eid="e48"
								className="absolute -bottom-2 left-1/2 -translate-x-1/2 w-1 h-1 rounded-full bg-foreground"
							/>
						</div>
						<span eid="e49" className="text-[10px] font-medium text-foreground">
							Home
						</span>
					</div>
					<div
						eid="e50"
						className="flex flex-col items-center gap-1 w-20 cursor-pointer opacity-60 hover:opacity-100 transition-opacity"
					>
						<Icon eid="e51" icon="solar:history-bold" className="size-6 text-muted-foreground" />
						<span eid="e52" className="text-[10px] font-medium text-muted-foreground">
							History
						</span>
					</div>
					<div
						eid="e53"
						className="flex flex-col items-center gap-1 w-20 cursor-pointer opacity-60 hover:opacity-100 transition-opacity"
					>
						<Icon eid="e54" icon="solar:settings-bold" className="size-6 text-muted-foreground" />
						<span eid="e55" className="text-[10px] font-medium text-muted-foreground">
							Settings
						</span>
					</div>
				</div>
			</nav>
		</div>
	);
}
