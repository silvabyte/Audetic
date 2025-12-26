import { Icon } from "@iconify/react";

export function Settings() {
	return (
		<div className="flex flex-col h-full bg-background text-foreground font-sans selection:bg-primary/20 overflow-hidden relative">
			<div className="absolute top-1/2 left-1/2 -translate-x-1/2 -translate-y-1/2 w-[500px] h-[500px] bg-primary/5 rounded-full blur-[100px] pointer-events-none" />
			<header className="flex items-center justify-between px-6 pt-12 pb-4 z-20 border-b border-border/50">
				<h1 className="text-2xl font-light tracking-tight text-foreground font-heading">
					Settings
				</h1>
			</header>
			<main className="flex-1 overflow-y-auto z-10 relative px-6 py-6">
				<div className="mb-8">
					<h2 className="text-xs font-medium tracking-widest text-muted-foreground uppercase mb-4 px-2">
						Transcription
					</h2>
					<div className="bg-card rounded-2xl border border-border/50 overflow-hidden">
						<div className="flex items-center justify-between px-4 py-4 border-b border-border/30">
							<div className="flex-1">
								<div className="text-sm font-medium text-foreground">Provider</div>
							</div>
							<div className="flex items-center gap-2">
								<span className="text-sm text-muted-foreground">Whisper</span>
								<Icon icon="solar:arrow-right-linear" className="size-4 text-muted-foreground" />
							</div>
						</div>
						<div className="flex items-center justify-between px-4 py-4 border-b border-border/30">
							<div className="flex-1">
								<div className="text-sm font-medium text-foreground">Language</div>
							</div>
							<div className="flex items-center gap-2">
								<span className="text-sm text-muted-foreground">English</span>
								<Icon icon="solar:arrow-right-linear" className="size-4 text-muted-foreground" />
							</div>
						</div>
						<div className="flex items-center justify-between px-4 py-4">
							<div className="flex-1">
								<div className="text-sm font-medium text-foreground">Quality</div>
							</div>
							<div className="flex items-center gap-2">
								<span className="text-sm text-muted-foreground">High</span>
								<Icon icon="solar:arrow-right-linear" className="size-4 text-muted-foreground" />
							</div>
						</div>
					</div>
				</div>
				<div className="mb-8">
					<h2 className="text-xs font-medium tracking-widest text-muted-foreground uppercase mb-4 px-2">
						Integrations
					</h2>
					<div className="bg-card rounded-2xl border border-border/50 overflow-hidden">
						<div className="flex items-center justify-between px-4 py-4 border-b border-border/30">
							<div className="flex-1">
								<div className="text-sm font-medium text-foreground">Notion</div>
								<div className="text-xs text-muted-foreground mt-0.5">Auto-sync transcripts</div>
							</div>
							<div className="relative inline-flex h-6 w-11 items-center rounded-full bg-primary">
								<span className="inline-block h-5 w-5 transform rounded-full bg-white shadow-sm transition translate-x-5" />
							</div>
						</div>
						<div className="flex items-center justify-between px-4 py-4 border-b border-border/30">
							<div className="flex-1">
								<div className="text-sm font-medium text-foreground">Dropbox</div>
								<div className="text-xs text-muted-foreground mt-0.5">Backup recordings</div>
							</div>
							<div className="relative inline-flex h-6 w-11 items-center rounded-full bg-muted">
								<span className="inline-block h-5 w-5 transform rounded-full bg-foreground/20 shadow-sm transition translate-x-1" />
							</div>
						</div>
						<div className="flex items-center justify-between px-4 py-4">
							<div className="flex-1">
								<div className="text-sm font-medium text-foreground">Slack</div>
								<div className="text-xs text-muted-foreground mt-0.5">Send to channels</div>
							</div>
							<div className="relative inline-flex h-6 w-11 items-center rounded-full bg-muted">
								<span className="inline-block h-5 w-5 transform rounded-full bg-foreground/20 shadow-sm transition translate-x-1" />
							</div>
						</div>
					</div>
				</div>
				<div className="mb-24">
					<h2 className="text-xs font-medium tracking-widest text-muted-foreground uppercase mb-4 px-2">
						Privacy & Storage
					</h2>
					<div className="bg-card rounded-2xl border border-border/50 overflow-hidden">
						<div className="flex items-center justify-between px-4 py-4 border-b border-border/30">
							<div className="flex-1">
								<div className="text-sm font-medium text-foreground">Auto-delete</div>
								<div className="text-xs text-muted-foreground mt-0.5">After 30 days</div>
							</div>
							<div className="relative inline-flex h-6 w-11 items-center rounded-full bg-primary">
								<span className="inline-block h-5 w-5 transform rounded-full bg-white shadow-sm transition translate-x-5" />
							</div>
						</div>
						<div className="flex items-center justify-between px-4 py-4 border-b border-border/30">
							<div className="flex-1">
								<div className="text-sm font-medium text-foreground">Local storage</div>
							</div>
							<div className="flex items-center gap-2">
								<span className="text-sm text-muted-foreground">2.4 GB</span>
								<Icon icon="solar:arrow-right-linear" className="size-4 text-muted-foreground" />
							</div>
						</div>
						<div className="flex items-center justify-between px-4 py-4 border-b border-border/30">
							<div className="flex-1">
								<div className="text-sm font-medium text-foreground">Backup</div>
								<div className="text-xs text-muted-foreground mt-0.5">Last: 2 hours ago</div>
							</div>
							<div className="relative inline-flex h-6 w-11 items-center rounded-full bg-primary">
								<span className="inline-block h-5 w-5 transform rounded-full bg-white shadow-sm transition translate-x-5" />
							</div>
						</div>
						<div className="flex items-center justify-between px-4 py-4">
							<div className="flex-1">
								<div className="text-sm font-medium text-primary">Clear cache</div>
								<div className="text-xs text-muted-foreground mt-0.5">Free up space</div>
							</div>
							<Icon icon="solar:arrow-right-linear" className="size-4 text-primary" />
						</div>
					</div>
				</div>
			</main>
			<nav className="border-t border-border bg-background/95 backdrop-blur-md pb-safe pt-2 z-30">
				<div className="flex items-center justify-around h-16">
					<div className="flex flex-col items-center gap-1 w-20 cursor-pointer opacity-60 hover:opacity-100 transition-opacity">
						<Icon icon="solar:home-2-linear" className="size-6 text-muted-foreground" />
						<span className="text-[10px] font-medium text-muted-foreground">Home</span>
					</div>
					<div className="flex flex-col items-center gap-1 w-20 cursor-pointer opacity-60 hover:opacity-100 transition-opacity">
						<Icon icon="solar:history-linear" className="size-6 text-muted-foreground" />
						<span className="text-[10px] font-medium text-muted-foreground">History</span>
					</div>
					<div className="flex flex-col items-center gap-1 w-20 cursor-pointer">
						<div className="relative">
							<Icon icon="solar:settings-bold" className="size-6 text-foreground" />
							<div className="absolute -bottom-2 left-1/2 -translate-x-1/2 w-1 h-1 rounded-full bg-primary" />
						</div>
						<span className="text-[10px] font-medium text-foreground">Settings</span>
					</div>
				</div>
			</nav>
		</div>
	);
}
