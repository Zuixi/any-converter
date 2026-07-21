import type { ReactNode, SVGProps } from "react";

type IconProps = SVGProps<SVGSVGElement>;

function NavIcon({ children, ...props }: IconProps & { children: ReactNode }) {
  return (
    <svg
      width="20"
      height="20"
      viewBox="0 0 48 48"
      fill="none"
      xmlns="http://www.w3.org/2000/svg"
      aria-hidden="true"
      {...props}
    >
      {children}
    </svg>
  );
}

export function DashboardIcon(props: IconProps) {
  return (
    <NavIcon {...props}>
      <path d="M6 6H20V20H6V6Z" stroke="currentColor" strokeWidth="4" strokeLinejoin="round" />
      <path d="M28 6H42V20H28V6Z" stroke="currentColor" strokeWidth="4" strokeLinejoin="round" />
      <path d="M6 28H20V42H6V28Z" stroke="currentColor" strokeWidth="4" strokeLinejoin="round" />
      <path d="M28 28H42V42H28V28Z" stroke="currentColor" strokeWidth="4" strokeLinejoin="round" />
    </NavIcon>
  );
}

/** Providers: interlocking rings (user-provided). */
export function ProvidersIcon(props: IconProps) {
  return (
    <NavIcon {...props}>
      <path
        d="M40.579 7.34863C44.3436 11.1132 39.9566 21.604 30.7803 30.7803C21.604 39.9566 11.1133 44.3436 7.34863 40.579C3.58399 36.8143 7.97101 26.3236 17.1473 17.1473C26.3236 7.97101 36.8143 3.58399 40.579 7.34863Z"
        stroke="currentColor"
        strokeWidth="4"
        strokeLinecap="butt"
        strokeLinejoin="round"
      />
      <path
        d="M7.48535 7.34863C3.72071 11.1132 8.10773 21.604 17.284 30.7803C26.4603 39.9566 36.951 44.3436 40.7157 40.579C44.4803 36.8143 40.0933 26.3236 30.917 17.1473C21.7407 7.97101 11.25 3.58399 7.48535 7.34863Z"
        stroke="currentColor"
        strokeWidth="4"
        strokeLinecap="butt"
        strokeLinejoin="round"
      />
    </NavIcon>
  );
}

export function RoutesIcon(props: IconProps) {
  return (
    <NavIcon {...props}>
      <path d="M10 38V10" stroke="currentColor" strokeWidth="4" strokeLinecap="round" />
      <path d="M10 10H30" stroke="currentColor" strokeWidth="4" strokeLinecap="round" />
      <path d="M30 10V26" stroke="currentColor" strokeWidth="4" strokeLinecap="round" />
      <path d="M30 26H42" stroke="currentColor" strokeWidth="4" strokeLinecap="round" />
      <circle cx="10" cy="38" r="3" fill="currentColor" />
      <circle cx="42" cy="26" r="3" fill="currentColor" />
    </NavIcon>
  );
}

/** Playground: frame + cross (user-provided first SVG). */
export function PlaygroundIcon(props: IconProps) {
  return (
    <NavIcon {...props}>
      <path d="M16 6H8C6.89543 6 6 6.89543 6 8V16" stroke="currentColor" strokeWidth="4" strokeLinecap="butt" strokeLinejoin="round" />
      <path d="M16 42H8C6.89543 42 6 41.1046 6 40V32" stroke="currentColor" strokeWidth="4" strokeLinecap="butt" strokeLinejoin="round" />
      <path d="M32 42H40C41.1046 42 42 41.1046 42 40V32" stroke="currentColor" strokeWidth="4" strokeLinecap="butt" strokeLinejoin="round" />
      <path d="M32 6H40C41.1046 6 42 6.89543 42 8V16" stroke="currentColor" strokeWidth="4" strokeLinecap="butt" strokeLinejoin="round" />
      <path d="M32 24L16 24" stroke="currentColor" strokeWidth="4" strokeLinecap="butt" strokeLinejoin="round" />
      <path d="M24 32L24 16" stroke="currentColor" strokeWidth="4" strokeLinecap="butt" strokeLinejoin="round" />
    </NavIcon>
  );
}

export function LogsIcon(props: IconProps) {
  return (
    <NavIcon {...props}>
      <path d="M10 8H38V40H10V8Z" stroke="currentColor" strokeWidth="4" strokeLinejoin="round" />
      <path d="M16 16H32" stroke="currentColor" strokeWidth="4" strokeLinecap="round" />
      <path d="M16 24H32" stroke="currentColor" strokeWidth="4" strokeLinecap="round" />
      <path d="M16 32H26" stroke="currentColor" strokeWidth="4" strokeLinecap="round" />
    </NavIcon>
  );
}

/** Usage: bar chart (user-provided). */
export function UsageIcon(props: IconProps) {
  return (
    <NavIcon {...props}>
      <path d="M6 6V42H42" stroke="currentColor" strokeWidth="4" strokeLinecap="butt" strokeLinejoin="round" />
      <path d="M14 30V34" stroke="currentColor" strokeWidth="4" strokeLinecap="butt" strokeLinejoin="round" />
      <path d="M22 22V34" stroke="currentColor" strokeWidth="4" strokeLinecap="butt" strokeLinejoin="round" />
      <path d="M30 6V34" stroke="currentColor" strokeWidth="4" strokeLinecap="butt" strokeLinejoin="round" />
      <path d="M38 14V34" stroke="currentColor" strokeWidth="4" strokeLinecap="butt" strokeLinejoin="round" />
    </NavIcon>
  );
}

export function SettingsIcon(props: IconProps) {
  return (
    <NavIcon {...props}>
      <path
        d="M24 30C27.3137 30 30 27.3137 30 24C30 20.6863 27.3137 18 24 18C20.6863 18 18 20.6863 18 24C18 27.3137 20.6863 30 24 30Z"
        stroke="currentColor"
        strokeWidth="4"
        strokeLinejoin="round"
      />
      <path
        d="M24 6V10M24 38V42M6 24H10M38 24H42M11 11L14 14M34 34L37 37M11 37L14 34M34 14L37 11"
        stroke="currentColor"
        strokeWidth="4"
        strokeLinecap="round"
      />
    </NavIcon>
  );
}

export function CollapseIcon(props: IconProps) {
  return (
    <NavIcon {...props}>
      <path d="M28 12L18 24L28 36" stroke="currentColor" strokeWidth="4" strokeLinecap="round" strokeLinejoin="round" />
    </NavIcon>
  );
}

export function ExpandIcon(props: IconProps) {
  return (
    <NavIcon {...props}>
      <path d="M20 12L30 24L20 36" stroke="currentColor" strokeWidth="4" strokeLinecap="round" strokeLinejoin="round" />
    </NavIcon>
  );
}
