import { Icon } from "@iconify/react";

export function History() {
	return (
		<div className="flex flex-col h-full bg-background text-foreground font-sans selection:bg-primary/20 overflow-hidden relative">
			<div className="absolute top-1/2 left-1/2 -translate-x-1/2 -translate-y-1/2 w-[500px] h-[500px] bg-primary/5 rounded-full blur-[100px] pointer-events-none" />
			<header className="flex items-center justify-between px-6 pt-12 pb-6 z-20">
				<h1 className="text-2xl font-semibold tracking-tight font-heading">History</h1>
				<button className="p-2 rounded-full hover:bg-card transition-colors">
					<Icon icon="solar:settings-bold" className="size-5 text-muted-foreground" />
				</button>
			</header>
			<div className="px-6 pb-4 z-20">
				<div className="relative">
					<Icon
						icon="solar:magnifer-linear"
						className="absolute left-4 top-1/2 -translate-y-1/2 size-5 text-muted-foreground"
					/>
					<input
						type="text"
						className="w-full pl-12 pr-4 py-3.5 rounded-xl bg-input border border-border text-foreground placeholder:text-muted-foreground focus:outline-none focus:ring-1 focus:ring-ring"
						placeholder="Search transcripts..."
					/>
				</div>
			</div>
			<div className="px-6 pb-4 z-20">
				<div className="flex gap-2 overflow-x-auto scrollbar-hide">
					<button className="px-4 py-2 rounded-full bg-primary text-primary-foreground text-sm font-medium whitespace-nowrap">
						All
					</button>
					<button className="px-4 py-2 rounded-full bg-muted text-muted-foreground text-sm font-medium whitespace-nowrap hover:bg-muted/80 transition-colors">
						Memos
					</button>
					<button className="px-4 py-2 rounded-full bg-muted text-muted-foreground text-sm font-medium whitespace-nowrap hover:bg-muted/80 transition-colors">
						Todos
					</button>
					<button className="px-4 py-2 rounded-full bg-muted text-muted-foreground text-sm font-medium whitespace-nowrap hover:bg-muted/80 transition-colors">
						Commands
					</button>
					<button className="px-4 py-2 rounded-full bg-muted text-muted-foreground text-sm font-medium whitespace-nowrap hover:bg-muted/80 transition-colors">
						Meetings
					</button>
				</div>
			</div>
			<div className="flex-1 overflow-y-auto px-6 pb-20 z-10">
				<div className="space-y-3">
					<div className="p-4 rounded-xl bg-[#1A1A1A] border border-border/30">
						<h3 className="text-base font-medium text-foreground mb-2">
							Meeting with design team about new features
						</h3>
						<div className="flex items-center gap-3 text-xs text-muted-foreground">
							<span className="flex items-center gap-1">
								<Icon icon="solar:calendar-linear" className="size-3.5" />
								Dec 15, 2024
							</span>
							<span className="flex items-center gap-1">
								<Icon icon="solar:clock-circle-linear" className="size-3.5" />
								12:34
							</span>
							<span className="px-2 py-0.5 rounded-full bg-muted/50 text-muted-foreground">
								Meeting
							</span>
						</div>
					</div>
					<div className="p-4 rounded-xl bg-[#1A1A1A] border border-border/30">
						<h3 className="text-base font-medium text-foreground mb-2">
							Quick memo about project timeline adjustments
						</h3>
						<div className="flex items-center gap-3 text-xs text-muted-foreground">
							<span className="flex items-center gap-1">
								<Icon icon="solar:calendar-linear" className="size-3.5" />
								Dec 14, 2024
							</span>
							<span className="flex items-center gap-1">
								<Icon icon="solar:clock-circle-linear" className="size-3.5" />
								2:15
							</span>
							<span className="px-2 py-0.5 rounded-full bg-muted/50 text-muted-foreground">
								Memo
							</span>
						</div>
					</div>
					<div className="p-4 rounded-xl bg-[#1A1A1A] border border-border/30">
						<h3 className="text-base font-medium text-foreground mb-2">
							Todo: Review pull requests and update documentation
						</h3>
						<div className="flex items-center gap-3 text-xs text-muted-foreground">
							<span className="flex items-center gap-1">
								<Icon icon="solar:calendar-linear" className="size-3.5" />
								Dec 14, 2024
							</span>
							<span className="flex items-center gap-1">
								<Icon icon="solar:clock-circle-linear" className="size-3.5" />
								5:42
							</span>
							<span className="px-2 py-0.5 rounded-full bg-muted/50 text-muted-foreground">
								Todo
							</span>
						</div>
					</div>
					<div className="p-4 rounded-xl bg-[#1A1A1A] border border-border/30">
						<h3 className="text-base font-medium text-foreground mb-2">
							Command: Schedule weekly sync for Monday morning
						</h3>
						<div className="flex items-center gap-3 text-xs text-muted-foreground">
							<span className="flex items-center gap-1">
								<Icon icon="solar:calendar-linear" className="size-3.5" />
								Dec 13, 2024
							</span>
							<span className="flex items-center gap-1">
								<Icon icon="solar:clock-circle-linear" className="size-3.5" />
								9:20
							</span>
							<span className="px-2 py-0.5 rounded-full bg-muted/50 text-muted-foreground">
								Command
							</span>
						</div>
					</div>
					<div className="p-4 rounded-xl bg-[#1A1A1A] border border-border/30">
						<h3 className="text-base font-medium text-foreground mb-2">
							Brainstorm session notes for Q1 marketing campaign
						</h3>
						<div className="flex items-center gap-3 text-xs text-muted-foreground">
							<span className="flex items-center gap-1">
								<Icon icon="solar:calendar-linear" className="size-3.5" />
								Dec 12, 2024
							</span>
							<span className="flex items-center gap-1">
								<Icon icon="solar:clock-circle-linear" className="size-3.5" />
								16:45
							</span>
							<span className="px-2 py-0.5 rounded-full bg-muted/50 text-muted-foreground">
								Meeting
							</span>
						</div>
					</div>
					<div className="p-4 rounded-xl bg-[#1A1A1A] border border-border/30">
						<h3 className="text-base font-medium text-foreground mb-2">
							Quick note about API integration requirements
						</h3>
						<div className="flex items-center gap-3 text-xs text-muted-foreground">
							<span className="flex items-center gap-1">
								<Icon icon="solar:calendar-linear" className="size-3.5" />
								Dec 11, 2024
							</span>
							<span className="flex items-center gap-1">
								<Icon icon="solar:clock-circle-linear" className="size-3.5" />
								11:30
							</span>
							<span className="px-2 py-0.5 rounded-full bg-muted/50 text-muted-foreground">
								Memo
							</span>
						</div>
					</div>
					<div className="p-4 rounded-xl bg-[#1A1A1A] border border-border/30">
						<h3 className="text-base font-medium text-foreground mb-2">
							Todo: Update user permissions and security settings
						</h3>
						<div className="flex items-center gap-3 text-xs text-muted-foreground">
							<span className="flex items-center gap-1">
								<Icon icon="solar:calendar-linear" className="size-3.5" />
								Dec 10, 2024
							</span>
							<span className="flex items-center gap-1">
								<Icon icon="solar:clock-circle-linear" className="size-3.5" />
								14:18
							</span>
							<span className="px-2 py-0.5 rounded-full bg-muted/50 text-muted-foreground">
								Todo
							</span>
						</div>
					</div>
					<div className="p-4 rounded-xl bg-[#1A1A1A] border border-border/30">
						<h3 className="text-base font-medium text-foreground mb-2">
							Client call recap and action items follow-up
						</h3>
						<div className="flex items-center gap-3 text-xs text-muted-foreground">
							<span className="flex items-center gap-1">
								<Icon icon="solar:calendar-linear" className="size-3.5" />
								Dec 9, 2024
							</span>
							<span className="flex items-center gap-1">
								<Icon icon="solar:clock-circle-linear" className="size-3.5" />
								10:05
							</span>
							<span className="px-2 py-0.5 rounded-full bg-muted/50 text-muted-foreground">
								Meeting
							</span>
						</div>
					</div>
				</div>
			</div>
			<nav className="border-t border-border bg-background/95 backdrop-blur-md pb-safe pt-2 z-30 fixed bottom-0 left-0 right-0">
				<div className="flex items-center justify-around h-16">
					<div className="flex flex-col items-center gap-1 w-20 cursor-pointer opacity-60 hover:opacity-100 transition-opacity">
						<Icon icon="solar:home-2-linear" className="size-6 text-muted-foreground" />
						<span className="text-[10px] font-medium text-muted-foreground">Home</span>
					</div>
					<div className="flex flex-col items-center gap-1 w-20 cursor-pointer">
						<div className="relative">
							<Icon icon="solar:history-bold" className="size-6 text-foreground" />
							<div className="absolute -bottom-2 left-1/2 -translate-x-1/2 w-1 h-1 rounded-full bg-primary" />
						</div>
						<span className="text-[10px] font-medium text-foreground">History</span>
					</div>
					<div className="flex flex-col items-center gap-1 w-20 cursor-pointer opacity-60 hover:opacity-100 transition-opacity">
						<Icon icon="solar:settings-linear" className="size-6 text-muted-foreground" />
						<span className="text-[10px] font-medium text-muted-foreground">Settings</span>
					</div>
				</div>
			</nav>
		</div>
	);
}
