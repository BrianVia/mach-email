<script lang="ts">
  type IconName = "search" | "star" | "send";

  let {
    name,
    size = 16,
    class: className = "",
    ariaLabel,
  }: {
    name: IconName;
    size?: number;
    class?: string;
    ariaLabel?: string;
  } = $props();
</script>

<svg
  xmlns="http://www.w3.org/2000/svg"
  width={size}
  height={size}
  viewBox="0 0 24 24"
  fill={name === "star" ? "currentColor" : "none"}
  stroke="currentColor"
  stroke-width="2"
  stroke-linecap="round"
  stroke-linejoin="round"
  class={className}
  aria-label={ariaLabel}
  aria-hidden={ariaLabel ? undefined : "true"}
>
  {#if name === "search"}
    <circle cx="11" cy="11" r="7" />
    <path d="m21 21-4.3-4.3" />
  {:else if name === "star"}
    <polygon points="12 2 15.09 8.26 22 9.27 17 14.14 18.18 21.02 12 17.77 5.82 21.02 7 14.14 2 9.27 8.91 8.26 12 2" />
  {:else}
    <path d="m22 2-7 20-4-9-9-4Z" />
    <path d="M22 2 11 13" />
  {/if}
</svg>
