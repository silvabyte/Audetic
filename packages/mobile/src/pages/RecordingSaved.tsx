import { Icon } from "@iconify/react";

export function RecordingSaved() {
	return (
		<div className="flex flex-col h-full bg-background text-foreground font-sans selection:bg-primary/20 overflow-hidden relative">
			<div className="flex-1 flex flex-col items-center justify-center px-6">
				<div className="flex flex-col items-center gap-6 max-w-xs">
					<div className="relative flex items-center justify-center size-32">
						<div className="absolute inset-0 rounded-full bg-green-500/10 animate-pulse" />
						<div className="absolute inset-4 rounded-full bg-green-500/20" />
						<div className="relative z-10 size-20 rounded-full bg-green-500/30 flex items-center justify-center">
							<Icon icon="solar:check-circle-bold" className="size-12 text-green-500" />
						</div>
					</div>
					<div className="flex flex-col items-center gap-2 text-center">
						<h1 className="text-2xl font-semibold text-foreground font-heading">Recording Saved</h1>
						<p className="text-sm text-muted-foreground">
							Your voice recording has been successfully saved and is ready to use
						</p>
					</div>
					<div className="flex flex-col gap-3 w-full mt-4">
						<button className="w-full py-3 px-4 bg-card text-card-foreground rounded-xl font-medium border border-border active:scale-95 transition-all">
							View Recording
						</button>
						<button className="w-full py-3 px-4 bg-primary text-primary-foreground rounded-xl font-semibold active:scale-95 transition-all">
							Create New
						</button>
					</div>
				</div>
			</div>
		</div>
	);
}
