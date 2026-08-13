<script lang="ts">
  import { onMount } from "svelte";

  type Command = { label: string; chord: string };

  let {
    v,
    commands,
    onInput,
    onMove,
    onEnter,
    onEsc,
    onPick,
  }: {
    v: { kind: "palette"; query: string; selected: number; background: unknown };
    commands: Command[];
    onInput: (query: string) => void;
    onMove: (delta: number) => void;
    onEnter: () => void;
    onEsc: () => void;
    onPick: (index: number) => void;
  } = $props();

  let input: HTMLInputElement;
  let filtered = $derived(
    commands.filter((command) =>
      command.label.toLocaleLowerCase().includes(v.query.trim().toLocaleLowerCase()),
    ),
  );
  let selected = $derived(Math.min(v.selected, Math.max(0, filtered.length - 1)));

  onMount(() => input?.focus());

  function chordKeys(chord: string): string[] {
    return chord.split(/\s+/).map((key) =>
      key.split("+").map((part) => {
        if (part === "ctrl") return "⌘";
        if (part === "shift") return "⇧";
        if (part === "enter") return "↵";
        if (part === "esc") return "⎋";
        return part.length === 1 ? part.toLocaleUpperCase() : part;
      }).join(""),
    );
  }
</script>

<div
  class="overlay"
  role="presentation"
  onclick={(event) => {
    if (event.target === event.currentTarget) onEsc();
  }}
>
  <section class="palette" aria-label="Command palette">
    <div class="input-row">
      <input
        bind:this={input}
        value={v.query}
        placeholder="Type a command…"
        aria-label="Filter commands"
        oninput={(event) => onInput(event.currentTarget.value)}
        onkeydown={(event) => {
          if (event.key === "Escape") { event.preventDefault(); onEsc(); }
          else if (event.key === "Enter") { event.preventDefault(); onEnter(); }
          else if (event.key === "ArrowDown") { event.preventDefault(); onMove(1); }
          else if (event.key === "ArrowUp") { event.preventDefault(); onMove(-1); }
        }}
      />
    </div>
    <div class="commands">
      {#each filtered as command, index (command.label)}
        <button
          type="button"
          class="command"
          class:selected={index === selected}
          onclick={() => onPick(index)}
        >
          <span>{command.label}</span>
          <span class="chord" aria-label={command.chord}>
            {#each chordKeys(command.chord) as key}
              <kbd>{key}</kbd>
            {/each}
          </span>
        </button>
      {/each}
    </div>
  </section>
</div>

<style>
  .overlay { position: absolute; inset: 0; z-index: 30; display: flex; align-items: flex-start; justify-content: center; background: color-mix(in oklab, var(--bg) 68%, transparent); backdrop-filter: blur(3px); }
  .palette { width: min(34rem, calc(100% - 32px)); max-height: 50vh; margin-top: 20vh; overflow: hidden; border: 1px solid var(--border); border-radius: 12px; background: var(--surface); box-shadow: 0 24px 70px color-mix(in oklab, black 35%, transparent); }
  .input-row { display: flex; align-items: center; padding: 14px 16px; border-bottom: 1px solid var(--border); }
  input { min-width: 0; flex: 1; border: 0; background: transparent; color: var(--text); font-size: 15px; outline: 0; }
  input::placeholder { color: var(--muted); }
  .commands { max-height: calc(50vh - 48px); overflow-y: auto; padding: 4px 0; }
  .command { display: flex; width: 100%; align-items: center; justify-content: space-between; gap: 16px; padding: 10px 16px; border: 0; background: transparent; color: inherit; font: inherit; font-size: 13.5px; text-align: left; cursor: pointer; }
  .command:not(.selected):hover { background: var(--hover); }
  .command.selected { background: color-mix(in oklab, var(--accent) 15%, transparent); }
  .chord { display: flex; flex-shrink: 0; gap: 4px; }
  kbd { min-width: 22px; padding: 2px 5px; border: 1px solid var(--border); border-radius: 4px; background: var(--surface-2); color: var(--muted); font-family: ui-monospace, SFMono-Regular, Menlo, Monaco, Consolas, monospace; font-size: 11px; line-height: 16px; text-align: center; }
</style>
