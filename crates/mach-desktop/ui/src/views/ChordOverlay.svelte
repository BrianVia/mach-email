<script lang="ts">
  let { show, buf, conts }: { show: boolean; buf: string; conts: { next: string; action_name: string }[] } = $props();
</script>

{#if show}
  <div class="overlay">
    <div class="prefix">Prefix <kbd>{buf}</kbd></div>
    <ul>
      {#each conts as continuation}
        <li><kbd>{continuation.next}</kbd><span>{continuation.action_name.replace(/_/g, " ")}</span></li>
      {/each}
    </ul>
  </div>
{/if}

<style>
  .overlay { position: absolute; bottom: 18px; left: 50%; z-index: 40; min-width: 280px; padding: 16px; transform: translateX(-50%); border: 1px solid var(--border); border-radius: 12px; background: var(--surface); box-shadow: 0 12px 36px color-mix(in oklab, black 30%, transparent); }
  .prefix { margin-bottom: 12px; color: var(--muted); font-size: 11px; letter-spacing: .08em; text-transform: uppercase; }
  ul { display: flex; flex-direction: column; gap: 6px; margin: 0; padding: 0; list-style: none; }
  li { display: flex; align-items: center; gap: 12px; color: var(--muted); font-size: 13px; }
  kbd { padding: 2px 5px; border: 1px solid var(--border); border-radius: 4px; background: var(--surface-2); color: var(--accent); font-family: var(--font-mono); font-size: 11px; text-transform: none; }
</style>
