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

/** Settings: gear (svgrepo.com, user-provided). Fill-based, own viewBox. */
export function SettingsIcon(props: IconProps) {
  return (
    <svg
      width="20"
      height="20"
      viewBox="0 0 54 54"
      fill="currentColor"
      xmlns="http://www.w3.org/2000/svg"
      aria-hidden="true"
      {...props}
    >
      <path d="M51.22,21h-5.052c-0.812,0-1.481-0.447-1.792-1.197s-0.153-1.54,0.42-2.114l3.572-3.571c0.525-0.525,0.814-1.224,0.814-1.966c0-0.743-0.289-1.441-0.814-1.967l-4.553-4.553c-1.05-1.05-2.881-1.052-3.933,0l-3.571,3.571c-0.574,0.573-1.366,0.733-2.114,0.421C33.447,9.313,33,8.644,33,7.832V2.78C33,1.247,31.753,0,30.22,0H23.78C22.247,0,21,1.247,21,2.78v5.052c0,0.812-0.447,1.481-1.197,1.792c-0.748,0.313-1.54,0.152-2.114-0.421l-3.571-3.571c-1.052-1.052-2.883-1.05-3.933,0l-4.553,4.553c-0.525,0.525-0.814,1.224-0.814,1.967c0,0.742,0.289,1.44,0.814,1.966l3.572,3.571c0.573,0.574,0.73,1.364,0.42,2.114S8.644,21,7.832,21H2.78C1.247,21,0,22.247,0,23.78v6.439C0,31.753,1.247,33,2.78,33h5.052c0.812,0,1.481,0.447,1.792,1.197s0.153,1.54-0.42,2.114l-3.572,3.571c-0.525,0.525-0.814,1.224-0.814,1.966c0,0.743,0.289,1.441,0.814,1.967l4.553,4.553c1.051,1.051,2.881,1.053,3.933,0l3.571-3.572c0.574-0.573,1.363-0.731,2.114-0.42c0.75,0.311,1.197,0.98,1.197,1.792v5.052c0,1.533,1.247,2.78,2.78,2.78h6.439c1.533,0,2.78-1.247,2.78-2.78v-5.052c0-0.812,0.447-1.481,1.197-1.792c0.751-0.312,1.54-0.153,2.114,0.42l3.571,3.572c1.052,1.052,2.883,1.05,3.933,0l4.553-4.553c0.525-0.525,0.814-1.224,0.814-1.967c0-0.742-0.289-1.44-0.814-1.966l-3.572-3.571c-0.573-0.574-0.73-1.364-0.42-2.114S45.356,33,46.168,33h5.052c1.533,0,2.78-1.247,2.78-2.78V23.78C54,22.247,52.753,21,51.22,21z M52,30.22C52,30.65,51.65,31,51.22,31h-5.052c-1.624,0-3.019,0.932-3.64,2.432c-0.622,1.5-0.295,3.146,0.854,4.294l3.572,3.571c0.305,0.305,0.305,0.8,0,1.104l-4.553,4.553c-0.304,0.304-0.799,0.306-1.104,0l-3.571-3.572c-1.149-1.149-2.794-1.474-4.294-0.854c-1.5,0.621-2.432,2.016-2.432,3.64v5.052C31,51.65,30.65,52,30.22,52H23.78C23.35,52,23,51.65,23,51.22v-5.052c0-1.624-0.932-3.019-2.432-3.64c-0.503-0.209-1.021-0.311-1.533-0.311c-1.014,0-1.997,0.4-2.761,1.164l-3.571,3.572c-0.306,0.306-0.801,0.304-1.104,0l-4.553-4.553c-0.305-0.305-0.305-0.8,0-1.104l3.572-3.571c1.148-1.148,1.476-2.794,0.854-4.294C10.851,31.932,9.456,31,7.832,31H2.78C2.35,31,2,30.65,2,30.22V23.78C2,23.35,2.35,23,2.78,23h5.052c1.624,0,3.019-0.932,3.64-2.432c0.622-1.5,0.295-3.146-0.854-4.294l-3.572-3.571c-0.305-0.305-0.305-0.8,0-1.104l4.553-4.553c0.304-0.305,0.799-0.305,1.104,0l3.571,3.571c1.147,1.147,2.792,1.476,4.294,0.854C22.068,10.851,23,9.456,23,7.832V2.78C23,2.35,23.35,2,23.78,2h6.439C30.65,2,31,2.35,31,2.78v5.052c0,1.624,0.932,3.019,2.432,3.64c1.502,0.622,3.146,0.294,4.294-0.854l3.571-3.571c0.306-0.305,0.801-0.305,1.104,0l4.553,4.553c0.305,0.305,0.305,0.8,0,1.104l-3.572,3.571c-1.148,1.148-1.476,2.794-0.854,4.294c0.621,1.5,2.016,2.432,3.64,2.432h5.052C51.65,23,52,23.35,52,23.78V30.22z" />
      <path d="M27,18c-4.963,0-9,4.037-9,9s4.037,9,9,9s9-4.037,9-9S31.963,18,27,18z M27,34c-3.859,0-7-3.141-7-7s3.141-7,7-7s7,3.141,7,7S30.859,34,27,34z" />
    </svg>
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

/** Agents: a plug meeting a socket (wiring an agent onto the gateway). */
export function AgentsIcon(props: IconProps) {
  return (
    <NavIcon {...props}>
      <path
        d="M28 6V16H22C16.4772 16 12 20.4772 12 26V30H6"
        stroke="currentColor"
        strokeWidth="4"
        strokeLinecap="round"
        strokeLinejoin="round"
      />
      <path d="M22 26H34V38H22V26Z" stroke="currentColor" strokeWidth="4" strokeLinejoin="round" />
      <path d="M6 30V38" stroke="currentColor" strokeWidth="4" strokeLinecap="round" />
      <path d="M2 34H10" stroke="currentColor" strokeWidth="4" strokeLinecap="round" />
      <path d="M38 20H46" stroke="currentColor" strokeWidth="4" strokeLinecap="round" />
    </NavIcon>
  );
}

/** Theme: sun — a disc with rays (light mode). */
export function SunIcon(props: IconProps) {
  return (
    <NavIcon {...props}>
      <circle cx="24" cy="24" r="8" stroke="currentColor" strokeWidth="4" />
      <path
        d="M24 4V10M24 38V44M4 24H10M38 24H44M10 10L14 14M34 34L38 38M10 38L14 34M34 14L38 10"
        stroke="currentColor"
        strokeWidth="4"
        strokeLinecap="round"
      />
    </NavIcon>
  );
}

/** Theme: moon — a crescent (dark mode). */
export function MoonIcon(props: IconProps) {
  return (
    <NavIcon {...props}>
      <path
        d="M40 28.5273C37.5892 29.4971 34.9566 30.0345 32.2029 30.0345C20.5014 30.0345 10.9655 20.4986 10.9655 8.79711C10.9655 6.04338 11.4765 3.41079 12.4463 1C6.6609 3.32847 2.31885 8.87993 2.31885 15.6605C2.31885 24.6326 9.83774 32 19.1592 32C25.9398 32 31.6728 27.9214 34.2674 21.911C35.2372 21.3742 36.5894 19.9419 37.1299 18.9721L40 28.5273Z"
        stroke="currentColor"
        strokeWidth="4"
        strokeLinejoin="round"
        transform="translate(2 6)"
      />
    </NavIcon>
  );
}

/** Theme: monitor — a screen on a stand (follow the system). */
export function SystemThemeIcon(props: IconProps) {
  return (
    <NavIcon {...props}>
      <rect x="6" y="8" width="36" height="24" rx="3" stroke="currentColor" strokeWidth="4" />
      <path d="M18 40H30M24 32V40" stroke="currentColor" strokeWidth="4" strokeLinecap="round" />
    </NavIcon>
  );
}

/** Brand: a tile with two opposing arrows — any format, either way. */
export function BrandMark(props: IconProps) {
  return (
    <NavIcon {...props}>
      <rect x="6" y="6" width="36" height="36" rx="10" stroke="currentColor" strokeWidth="4" />
      <path d="M15 20H32M27 15L32 20L27 25" stroke="currentColor" strokeWidth="4" strokeLinecap="round" strokeLinejoin="round" />
      <path d="M33 28H16M21 23L16 28L21 33" stroke="currentColor" strokeWidth="4" strokeLinecap="round" strokeLinejoin="round" />
    </NavIcon>
  );
}
