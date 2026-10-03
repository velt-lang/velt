// @jsxImportSource ./_jsx_test_provider

// A component in a `.tsx` module, imported by `ts_modules.vlt`.

export function Badge(props: { label: string; count: number }): JSX.Element {
  return (
    <span class="badge">
      {props.label}: {props.count}
    </span>
  );
}
