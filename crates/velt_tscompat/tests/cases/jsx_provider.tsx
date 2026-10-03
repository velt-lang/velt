// JSX without a `jsxImportSource` uses `velt:jsx`, which has no TypeScript runtime.

export function Badge(props: { label: string }): JSX.Element {
  return <span class="badge">{props.label}</span>; //~ jsx-provider
}

export function Pair(props: { a: string; b: string }): JSX.Element {
  return (
    <p>
      {props.a} {props.b}
    </p>
  );
}
