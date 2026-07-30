// Sender-avatar helpers. Initials + a stable per-sender background color
// hashed from the email address. No external image fetching — fast and
// deterministic across re-renders.

const AVATAR_HUES = [
  210, 190, 170, 150, 130, 110, 90, 70, 50, 30, 10, 350, 330, 310, 290, 270, 250, 230,
];

/**
 * Extract initials from a "Name <email>" or "email" string.
 * "Andrew Busse <a@…>" → "AB"; "noreply@github.com" → "GH".
 */
export function initialsFor(from: string): string {
  const name = from.replace(/<[^>]+>/, "").trim();
  if (name) {
    const parts = name.split(/[\s.,_-]+/).filter(Boolean);
    if (parts.length >= 2) return (parts[0][0] + parts[parts.length - 1][0]).toUpperCase();
    if (parts.length === 1) return parts[0].slice(0, 2).toUpperCase();
  }
  // Email-only fallback.
  const m = from.match(/[\w.+-]+@([\w-]+)/);
  if (m) return m[1].slice(0, 2).toUpperCase();
  return "??";
}

/** Stable color from a string (FNV-1a hash → hue). */
export function avatarColor(seed: string): string {
  let h = 2166136261;
  for (let i = 0; i < seed.length; i++) {
    h ^= seed.charCodeAt(i);
    h = Math.imul(h, 16777619);
  }
  const hue = AVATAR_HUES[Math.abs(h) % AVATAR_HUES.length];
  return `hsl(${hue}, 38%, 38%)`;
}
