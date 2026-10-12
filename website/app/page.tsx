import { site } from "@/site.config";
import { InstallCommand } from "./install-command";

const socialLabels: Record<keyof typeof site.socials, string> = {
  x: "X",
  linkedin: "LinkedIn",
  github: "GitHub",
  website: "Website",
};

export default function Home() {
  const socials = (Object.keys(site.socials) as (keyof typeof site.socials)[]).filter(
    (k) => site.socials[k],
  );

  return (
    <div className="flex min-h-full flex-1 flex-col">
      <header className="mx-auto flex w-full max-w-5xl items-center justify-between px-6 py-6">
        <span className="font-mono text-lg font-semibold tracking-tight">{site.name}</span>
        <a
          href={site.github}
          className="rounded-lg border border-border px-3 py-1.5 text-sm transition-colors hover:bg-foreground/5"
        >
          GitHub
        </a>
      </header>

      <main className="mx-auto flex w-full max-w-5xl flex-1 flex-col items-center px-6 pb-16 pt-10 text-center sm:pt-20">
        <span className="mb-6 rounded-full border border-border px-3 py-1 font-mono text-xs text-muted">
          {site.version} · macOS & Linux · open source
        </span>
        <h1 className="max-w-3xl text-4xl font-semibold tracking-tight text-balance sm:text-6xl">
          {site.tagline}
        </h1>
        <p className="mt-6 max-w-2xl text-base text-muted text-pretty sm:text-lg">{site.description}</p>

        <div className="mt-10 flex w-full justify-center">
          <InstallCommand options={site.install} />
        </div>

        <div className="mt-6 flex flex-wrap justify-center gap-3">
          <a
            href={site.github}
            className="rounded-lg bg-foreground px-5 py-2.5 text-sm font-medium text-background transition-opacity hover:opacity-90"
          >
            View on GitHub
          </a>
          <a
            href={`${site.github}#quick-start-2-minutes`}
            className="rounded-lg border border-border px-5 py-2.5 text-sm font-medium transition-colors hover:bg-foreground/5"
          >
            Quick start
          </a>
        </div>

        <TerminalPreview />
      </main>

      <footer className="mx-auto flex w-full max-w-5xl flex-col items-center justify-between gap-4 border-t border-border px-6 py-8 text-sm text-muted sm:flex-row">
        <span>
          {site.name} · MIT / Apache-2.0
        </span>
        <nav className="flex gap-5" aria-label="Social links">
          {socials.map((k) => (
            <a key={k} href={site.socials[k]} className="transition-colors hover:text-foreground">
              {socialLabels[k]}
            </a>
          ))}
        </nav>
      </footer>
    </div>
  );
}

/** Static mock of oUI: two profile tabs, two different accounts. */
function TerminalPreview() {
  return (
    <div className="mt-16 w-full max-w-3xl overflow-hidden rounded-xl border border-border bg-surface text-left font-mono text-[11px] sm:text-[13px] shadow-2xl shadow-black/20">
      <div className="flex items-center gap-1.5 border-b border-border px-3 py-2">
        <span className="size-3 rounded-full bg-[#ff5f57]" />
        <span className="size-3 rounded-full bg-[#febc2e]" />
        <span className="size-3 rounded-full bg-[#28c840]" />
      </div>
      <div className="flex items-center gap-1 whitespace-nowrap bg-black/20 px-2 py-1">
        <span className="rounded bg-accent px-2 text-black">oUI</span>
        <span className="rounded bg-emerald-500 px-2 font-semibold text-black">1 work</span>
        <span className="rounded px-2 text-fuchsia-400">2 personal</span>
        <span className="px-2 text-muted">+</span>
        <span className="ml-auto hidden pr-1 text-muted sm:inline">session main</span>
      </div>
      <div className="grid grid-cols-2 divide-x divide-emerald-500/60">
        <pre className="overflow-hidden p-3 leading-6">
          <span className="text-muted">$ </span>claude{"\n"}
          <span className="text-emerald-400">✓ Logged in as you@company.com</span>
          {"\n"}
          <span className="text-muted">  Claude Max · work</span>
          {"\n"}
          <span className="text-muted">$ </span>railway whoami{"\n"}
          you@company.com
        </pre>
        <pre className="overflow-hidden p-3 leading-6">
          <span className="text-muted">$ </span>gh auth status{"\n"}
          <span className="text-emerald-400">✓ Logged in as you-work</span>
          {"\n"}
          <span className="text-muted">$ </span>
          <span className="inline-block h-4 w-2 translate-y-0.5 bg-foreground/80" />
        </pre>
      </div>
      <div className="flex items-center gap-2 bg-black/20 px-3 py-1 text-xs text-muted">
        <span className="text-foreground">main</span>│<span className="text-emerald-400">● work</span>
        <span className="ml-auto hidden sm:inline">^b c tab · %/&quot; split · d close pane · D detach</span>
      </div>
    </div>
  );
}
