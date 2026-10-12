"use client";

import { useState } from "react";

type Option = { label: string; command: string };

export function InstallCommand({ options }: { options: Option[] }) {
  const [active, setActive] = useState(0);
  const [copied, setCopied] = useState(false);
  const command = options[active].command;

  async function copy() {
    try {
      await navigator.clipboard.writeText(command);
      setCopied(true);
      setTimeout(() => setCopied(false), 1500);
    } catch {
      // Clipboard unavailable (e.g. insecure context): the command stays selectable.
    }
  }

  return (
    <div className="w-full max-w-xl text-left">
      <div className="mb-2 flex gap-1" role="tablist" aria-label="Install method">
        {options.map((o, i) => (
          <button
            key={o.label}
            role="tab"
            aria-selected={i === active}
            onClick={() => setActive(i)}
            className={`rounded-md px-3 py-1 text-xs font-medium transition-colors ${
              i === active ? "bg-foreground/10 text-foreground" : "text-muted hover:text-foreground"
            }`}
          >
            {o.label}
          </button>
        ))}
      </div>
      <div className="flex items-center gap-3 rounded-xl border border-border bg-surface px-4 py-3 font-mono text-sm">
        <span className="select-none text-accent">$</span>
        <code className="flex-1 overflow-x-auto whitespace-nowrap">{command}</code>
        <button
          onClick={copy}
          aria-label="Copy install command"
          className="shrink-0 rounded-md border border-border px-2.5 py-1 text-xs text-muted transition-colors hover:text-foreground"
        >
          {copied ? "Copied" : "Copy"}
        </button>
      </div>
    </div>
  );
}
