import { Icon } from "@iconify/react";

export function TranscriptDetail() {
	return (
		<div className="flex flex-col h-full bg-background text-foreground font-sans selection:bg-primary/20 overflow-hidden relative">
			<div className="absolute top-1/2 left-1/2 -translate-x-1/2 -translate-y-1/2 w-[500px] h-[500px] bg-primary/5 rounded-full blur-[100px] pointer-events-none" />
			<header className="flex items-center justify-between px-4 py-4 border-b border-border/50 z-20 bg-background/95 backdrop-blur-md">
				<button className="flex items-center justify-center size-10 rounded-full hover:bg-card/50 transition-colors active:scale-95">
					<Icon icon="solar:arrow-left-linear" className="size-6 text-foreground" />
				</button>
				<div className="flex-1 flex flex-col items-center px-4">
					<h1 className="text-base font-semibold font-heading text-foreground">Meeting Notes</h1>
					<span className="text-xs text-muted-foreground">Jan 15, 2025</span>
				</div>
				<div className="flex items-center gap-2">
					<button className="flex items-center justify-center size-10 rounded-full hover:bg-card/50 transition-colors active:scale-95">
						<Icon icon="solar:share-linear" className="size-5 text-muted-foreground" />
					</button>
					<button className="flex items-center justify-center size-10 rounded-full hover:bg-destructive/20 transition-colors active:scale-95">
						<Icon
							icon="solar:trash-bin-minimalistic-linear"
							className="size-5 text-muted-foreground"
						/>
					</button>
				</div>
			</header>
			<div className="px-6 py-4 bg-card/30 border-b border-border/30 z-10">
				<div className="flex flex-wrap items-center gap-4 text-xs">
					<div className="flex items-center gap-2">
						<Icon icon="solar:calendar-linear" className="size-4 text-muted-foreground" />
						<span className="text-muted-foreground">Jan 15, 2025 • 2:30 PM</span>
					</div>
					<div className="flex items-center gap-2">
						<Icon icon="solar:clock-circle-linear" className="size-4 text-muted-foreground" />
						<span className="text-muted-foreground">4:12</span>
					</div>
					<div className="flex items-center gap-2">
						<Icon icon="solar:microphone-3-linear" className="size-4 text-muted-foreground" />
						<span className="text-muted-foreground">Voice Memo</span>
					</div>
				</div>
				<div className="flex flex-wrap gap-2 mt-3">
					<div className="px-3 py-1 rounded-full bg-card border border-border/50 text-xs text-muted-foreground">
						#work
					</div>
					<div className="px-3 py-1 rounded-full bg-card border border-border/50 text-xs text-muted-foreground">
						#meeting
					</div>
					<div className="px-3 py-1 rounded-full bg-card border border-border/50 text-xs text-muted-foreground">
						#important
					</div>
				</div>
			</div>
			<main className="flex-1 overflow-y-auto px-6 py-6 z-10">
				<div className="max-w-2xl mx-auto">
					<p className="text-[15px] leading-[1.8] text-foreground/90 select-text">
						Today's meeting covered several key points regarding the Q1 product launch. First, we
						discussed the timeline and agreed that we need to push the release date back by two
						weeks to ensure proper testing. The development team highlighted some technical
						challenges with the new authentication system that need more time to resolve.
					</p>
					<p className="text-[15px] leading-[1.8] text-foreground/90 select-text mt-6">
						Sarah from marketing presented the campaign strategy, which looks really promising.
						We'll be focusing on social media engagement and influencer partnerships. The budget was
						approved for an additional $50,000 to support this initiative.
					</p>
					<p className="text-[15px] leading-[1.8] text-foreground/90 select-text mt-6">
						Action items were assigned as follows: John will coordinate with the QA team to set up
						the testing environment by next Monday. Maria will finalize the marketing materials and
						share them with the team for review by Wednesday. I'm responsible for updating the
						project timeline and communicating the new launch date to stakeholders.
					</p>
					<p className="text-[15px] leading-[1.8] text-foreground/90 select-text mt-6">
						We also touched on the competitive landscape. There are two new entrants in our market
						segment that we need to keep an eye on. The product team will conduct a competitive
						analysis and present findings in next week's meeting.
					</p>
					<p className="text-[15px] leading-[1.8] text-foreground/90 select-text mt-6">
						Overall, the meeting was productive and everyone is aligned on the path forward. Next
						review is scheduled for January 22nd at the same time.
					</p>
				</div>
			</main>
			<div className="border-t border-border/50 bg-background/95 backdrop-blur-md px-6 py-3 z-20">
				<div className="flex items-center justify-around max-w-md mx-auto">
					<button className="flex flex-col items-center gap-1.5 p-2 hover:bg-card/30 rounded-xl transition-colors active:scale-95">
						<Icon icon="solar:pen-linear" className="size-6 text-muted-foreground" />
						<span className="text-[10px] font-medium text-muted-foreground tracking-wide uppercase">
							Edit
						</span>
					</button>
					<button className="flex flex-col items-center gap-1.5 p-2 hover:bg-card/30 rounded-xl transition-colors active:scale-95">
						<Icon icon="solar:tag-linear" className="size-6 text-muted-foreground" />
						<span className="text-[10px] font-medium text-muted-foreground tracking-wide uppercase">
							Add Tag
						</span>
					</button>
					<button className="flex flex-col items-center gap-1.5 p-2 hover:bg-card/30 rounded-xl transition-colors active:scale-95">
						<Icon icon="solar:export-linear" className="size-6 text-muted-foreground" />
						<span className="text-[10px] font-medium text-muted-foreground tracking-wide uppercase">
							Export
						</span>
					</button>
					<button className="flex flex-col items-center gap-1.5 p-2 hover:bg-card/30 rounded-xl transition-colors active:scale-95">
						<Icon icon="solar:copy-linear" className="size-6 text-muted-foreground" />
						<span className="text-[10px] font-medium text-muted-foreground tracking-wide uppercase">
							Copy
						</span>
					</button>
				</div>
			</div>
		</div>
	);
}
