// Small hand-drawn-ish line icons (stroke = currentColor).
import type { SVGProps } from "react";

type P = SVGProps<SVGSVGElement> & { size?: number };

function base({ size = 18, ...rest }: P) {
  return {
    width: size,
    height: size,
    viewBox: "0 0 24 24",
    fill: "none",
    stroke: "currentColor",
    strokeWidth: 2,
    strokeLinecap: "round" as const,
    strokeLinejoin: "round" as const,
    "aria-hidden": true,
    ...rest,
  };
}

export const Paw = ({ size = 18, ...rest }: P) => (
  <svg width={size} height={size} viewBox="0 0 24 24" fill="currentColor" aria-hidden {...rest}>
    <ellipse cx="6" cy="10" rx="2.2" ry="2.8" />
    <ellipse cx="10" cy="6" rx="2.2" ry="2.9" />
    <ellipse cx="14.5" cy="6" rx="2.2" ry="2.9" />
    <ellipse cx="18.5" cy="10" rx="2.2" ry="2.8" />
    <path d="M12.2 11.5c-3.3 0-6.2 3.6-6.2 6.2 0 1.8 1.5 2.8 3.2 2.8 1.2 0 2-.6 3-.6s1.8.6 3 .6c1.7 0 3.2-1 3.2-2.8 0-2.6-2.9-6.2-6.2-6.2Z" />
  </svg>
);

export const Home = (p: P) => (
  <svg {...base(p)}>
    <path d="M3 11.5 12 4l9 7.5" />
    <path d="M5.5 10v9.5h13V10" />
    <path d="M10 19.5v-5h4v5" />
  </svg>
);

export const Plus = (p: P) => (
  <svg {...base(p)}>
    <path d="M12 5v14M5 12h14" />
  </svg>
);

export const Chat = (p: P) => (
  <svg {...base(p)}>
    <path d="M4.5 5.5h15a1.5 1.5 0 0 1 1.5 1.5v9a1.5 1.5 0 0 1-1.5 1.5H10l-4.5 3.5V17.5h-1A1.5 1.5 0 0 1 3 16V7a1.5 1.5 0 0 1 1.5-1.5Z" />
    <path d="M8 10.5h8M8 13.5h5" />
  </svg>
);

export const Bell = (p: P) => (
  <svg {...base(p)}>
    <path d="M6 16.5V11a6 6 0 1 1 12 0v5.5l1.5 2h-15Z" />
    <path d="M10 20.5a2 2 0 0 0 4 0" />
  </svg>
);

export const Bone = (p: P) => (
  <svg {...base(p)}>
    <path d="M8.5 9.5 14.5 15.5M6.2 11.6a2.3 2.3 0 1 1-1.6-3.9 2.3 2.3 0 1 1 3.9-1.6l7.3 7.3a2.3 2.3 0 1 1 1.6 3.9 2.3 2.3 0 1 1-3.9 1.6Z" />
  </svg>
);

export const Shield = (p: P) => (
  <svg {...base(p)}>
    <path d="M12 3.5 19 6v5.5c0 4.3-3 7.7-7 9-4-1.3-7-4.7-7-9V6Z" />
    <path d="m9 12 2.2 2.2L15.5 10" />
  </svg>
);

export const Gear = (p: P) => (
  <svg {...base(p)}>
    <circle cx="12" cy="12" r="3" />
    <path d="M19.4 15a1.7 1.7 0 0 0 .3 1.8l.1.1a2 2 0 1 1-2.8 2.8l-.1-.1a1.7 1.7 0 0 0-1.8-.3 1.7 1.7 0 0 0-1 1.5V21a2 2 0 1 1-4 0v-.1a1.7 1.7 0 0 0-1.1-1.6 1.7 1.7 0 0 0-1.8.3l-.1.1a2 2 0 1 1-2.8-2.8l.1-.1a1.7 1.7 0 0 0 .3-1.8 1.7 1.7 0 0 0-1.5-1H3a2 2 0 1 1 0-4h.1a1.7 1.7 0 0 0 1.6-1.1 1.7 1.7 0 0 0-.3-1.8l-.1-.1a2 2 0 1 1 2.8-2.8l.1.1a1.7 1.7 0 0 0 1.8.3H9a1.7 1.7 0 0 0 1-1.5V3a2 2 0 1 1 4 0v.1a1.7 1.7 0 0 0 1 1.5 1.7 1.7 0 0 0 1.8-.3l.1-.1a2 2 0 1 1 2.8 2.8l-.1.1a1.7 1.7 0 0 0-.3 1.8V9a1.7 1.7 0 0 0 1.5 1H21a2 2 0 1 1 0 4h-.1a1.7 1.7 0 0 0-1.5 1Z" />
  </svg>
);

export const Folder = (p: P) => (
  <svg {...base(p)}>
    <path d="M3.5 7.5A1.5 1.5 0 0 1 5 6h4l2 2h8a1.5 1.5 0 0 1 1.5 1.5v8A1.5 1.5 0 0 1 19 19H5a1.5 1.5 0 0 1-1.5-1.5Z" />
  </svg>
);

export const Send = (p: P) => (
  <svg {...base(p)}>
    <path d="M4 12 20 4l-6 16-2.5-6.5Z" />
    <path d="M11.5 13.5 20 4" />
  </svg>
);

export const Stop = (p: P) => (
  <svg {...base(p)}>
    <rect x="6" y="6" width="12" height="12" rx="2.5" />
  </svg>
);

export const ArrowUp = (p: P) => (
  <svg {...base(p)}>
    <path d="m6 14 6-6 6 6" />
  </svg>
);

export const ArrowDown = (p: P) => (
  <svg {...base(p)}>
    <path d="m6 10 6 6 6-6" />
  </svg>
);

export const Trash = (p: P) => (
  <svg {...base(p)}>
    <path d="M4.5 7h15M9.5 7V4.5h5V7M6.5 7l1 12.5h9l1-12.5" />
  </svg>
);

export const Monitor = (p: P) => (
  <svg {...base(p)}>
    <rect x="3" y="4" width="18" height="12" rx="2" />
    <path d="M8.5 20h7" />
    <path d="M12 16v4" />
  </svg>
);

/** Two screens: the paired machines. */
export const Machines = (p: P) => (
  <svg {...base(p)}>
    <rect x="2.5" y="5" width="12" height="8.5" rx="1.6" />
    <path d="M6 17h5M8.5 13.5V17" />
    <rect x="16" y="8" width="5.5" height="10" rx="1.4" />
    <path d="M18.75 15.5h.01" />
  </svg>
);

export const Copy = (p: P) => (
  <svg {...base(p)}>
    <rect x="8.5" y="8.5" width="11" height="11" rx="2" />
    <path d="M15.5 8.5V6a1.5 1.5 0 0 0-1.5-1.5H6A1.5 1.5 0 0 0 4.5 6v8A1.5 1.5 0 0 0 6 15.5h2.5" />
  </svg>
);

/** A phone (OS unknown). */
export const Phone = (p: P) => (
  <svg {...base(p)}>
    <rect x="6.5" y="2.5" width="11" height="19" rx="2.4" />
    <path d="M11 18.5h2" />
  </svg>
);

/** An iPhone-style phone: a notch at the top, no buttons on the face. Not a brand logo. */
export const PhoneIos = (p: P) => (
  <svg {...base(p)}>
    <rect x="6.5" y="2.5" width="11" height="19" rx="3" />
    <path d="M10.5 5h3" />
  </svg>
);

/** An Android-style phone: a camera dot at the top and a navigation bar at the bottom. Not a brand logo. */
export const PhoneAndroid = (p: P) => (
  <svg {...base(p)}>
    <rect x="6.5" y="2.5" width="11" height="19" rx="1.8" />
    <path d="M12 5.2h.01" />
    <path d="M9.5 18.5h5" />
  </svg>
);
