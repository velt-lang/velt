//! Operators that call operator methods (#2050): go to definition and hover on the operator
//! find the method it calls.

use super::client::{at, pos_of, uri, Client};

const APP: &str = r#"class V {
  constructor(public x: f64) {}
  [Symbol.add](o: V): V {
    return new V(this.x + o.x);
  }
  [Symbol.neg](): V {
    return new V(-this.x);
  }
  [Symbol.index](i: usize): f64 {
    return this.x;
  }
}
extend V {
  [Symbol.addAssign](o: V): void {
    this.x += o.x;
  }
}

function main() {
  let a = new V(1);
  const b = new V(2);
  const c = a + b;
  const d = -c;
  a += d;
  console.log(a[0]);
}
"#;

/// The line and column of the definition at `needle` (+`delta`).
fn definition(client: &mut Client, app: &lsp_types::Url, needle: &str, delta: usize) -> (u32, u32) {
    let (line, col) = pos_of(APP, needle, delta);
    let loc = client.request("textDocument/definition", at(app, line, col));
    assert!(!loc.is_null(), "no definition for `{needle}`");
    let start = &loc["range"]["start"];
    (
        start["line"].as_u64().unwrap() as u32,
        start["character"].as_u64().unwrap() as u32,
    )
}

#[test]
fn an_operator_leads_to_its_method() {
    let mut client = Client::start();
    let app = uri("operators_app.vlt");
    client.open(&app, APP);
    let decl = |needle: &str| pos_of(APP, needle, 0);
    assert_eq!(
        definition(&mut client, &app, "a + b", 2),
        decl("[Symbol.add]")
    );
    assert_eq!(definition(&mut client, &app, "-c", 0), decl("[Symbol.neg]"));
    assert_eq!(
        definition(&mut client, &app, "a += d", 2),
        decl("[Symbol.addAssign]")
    );
    assert_eq!(
        definition(&mut client, &app, "a[0]", 2),
        decl("[Symbol.index]")
    );
    let hover = client.request("textDocument/hover", {
        let (line, col) = pos_of(APP, "a + b", 2);
        at(&app, line, col)
    });
    let text = hover["contents"]["value"].as_str().unwrap_or_default();
    assert_eq!(text, "```velt\n(method) V.[Symbol.add](o: V): V\n```");
    client.shutdown();
}
