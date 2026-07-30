// Inline SVG icons. Hand-picked from Lucide / Heroicons — kept inline so
// they tree-shake to exactly what's used and don't need a runtime lib.

import type { JSX } from "solid-js";

type IconProps = JSX.SvgSVGAttributes<SVGSVGElement> & { size?: number };

function Base(props: IconProps & { children: JSX.Element }) {
  return (
    <svg
      xmlns="http://www.w3.org/2000/svg"
      width={props.size ?? 16}
      height={props.size ?? 16}
      viewBox="0 0 24 24"
      fill="none"
      stroke="currentColor"
      stroke-width="2"
      stroke-linecap="round"
      stroke-linejoin="round"
      {...props}
    >
      {props.children}
    </svg>
  );
}

export const Search = (p: IconProps) => (
  <Base {...p}><circle cx="11" cy="11" r="7" /><path d="m21 21-4.3-4.3" /></Base>
);
export const Star = (p: IconProps) => (
  <Base {...p} fill="currentColor"><polygon points="12 2 15.09 8.26 22 9.27 17 14.14 18.18 21.02 12 17.77 5.82 21.02 7 14.14 2 9.27 8.91 8.26 12 2" /></Base>
);
export const Edit = (p: IconProps) => (
  <Base {...p}><path d="M12 20h9" /><path d="M16.5 3.5a2.121 2.121 0 0 1 3 3L7 19l-4 1 1-4Z" /></Base>
);
export const ArrowLeft = (p: IconProps) => (
  <Base {...p}><path d="m12 19-7-7 7-7" /><path d="M19 12H5" /></Base>
);
export const Inbox = (p: IconProps) => (
  <Base {...p}><polyline points="22 12 16 12 14 15 10 15 8 12 2 12" /><path d="M5.45 5.11 2 12v6a2 2 0 0 0 2 2h16a2 2 0 0 0 2-2v-6l-3.45-6.89A2 2 0 0 0 16.76 4H7.24a2 2 0 0 0-1.79 1.11Z" /></Base>
);
export const Archive = (p: IconProps) => (
  <Base {...p}><rect width="20" height="5" x="2" y="3" rx="1" /><path d="M4 8v11a2 2 0 0 0 2 2h12a2 2 0 0 0 2-2V8" /><path d="M10 12h4" /></Base>
);
export const Trash = (p: IconProps) => (
  <Base {...p}><path d="M3 6h18" /><path d="M19 6v14a2 2 0 0 1-2 2H7a2 2 0 0 1-2-2V6" /><path d="M8 6V4a2 2 0 0 1 2-2h4a2 2 0 0 1 2 2v2" /></Base>
);
export const Send = (p: IconProps) => (
  <Base {...p}><path d="m22 2-7 20-4-9-9-4Z" /><path d="M22 2 11 13" /></Base>
);
export const Dot = (p: IconProps) => (
  <Base {...p} fill="currentColor" stroke="none"><circle cx="12" cy="12" r="6" /></Base>
);
