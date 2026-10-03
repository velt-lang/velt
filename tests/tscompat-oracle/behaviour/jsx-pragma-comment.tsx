// `tsc` takes the JSX pragma only from a block comment and ignores a line comment, so the client
// compiles this file with its tsconfig's provider while Velt uses the one named here. The named
// provider doesn't exist: `tsc` accepts the file only because it ignores the pragma, and reports
// the missing runtime once the fix makes the pragma a block comment (oracle.rs, `IGNORED`).
// @jsxImportSource ./missing-provider

export function Badge(props: { label: string }): JSX.Element {
  return <span class="badge">{props.label}</span>;
}
