// Small line icons, drawn in the text color.
const common = { viewBox: "0 0 24 24", fill: "none", stroke: "currentColor", strokeWidth: 1.8, strokeLinecap: "round", strokeLinejoin: "round", "aria-hidden": true } as const;

export const CloseIcon = () => (
  <svg {...common}>
    <path d="M6 6l12 12M18 6L6 18" />
  </svg>
);
export const PlusIcon = () => (
  <svg {...common}>
    <path d="M12 5v14M5 12h14" />
  </svg>
);
export const MoreIcon = () => (
  <svg {...common} strokeWidth={2.6}>
    <path d="M5 12h.01M12 12h.01M19 12h.01" />
  </svg>
);
export const ChevronIcon = () => (
  <svg {...common}>
    <path d="M6 9l6 6 6-6" />
  </svg>
);
export const MenuIcon = () => (
  <svg {...common}>
    <path d="M4 7h16M4 12h16M4 17h16" />
  </svg>
);
export const CheckIcon = () => (
  <svg {...common}>
    <path d="M5 12.5l4.5 4.5L19 7.5" />
  </svg>
);
