//! Snapshot tests for JSX: tags and attributes, children reflow under React's whitespace rules,
//! parentheses around multi-line elements, comments inside JSX, conditionals, and the cases
//! where the formatter must keep what an element renders (spaces next to tags and `{" "}`,
//! runs of spaces, entities, self-closing tags). Expected outputs are prettier's, except where a
//! test says otherwise.
//! Every case is also checked for idempotency, AST preservation and comment preservation.

mod common;

use velt_fmt::format_source;

#[track_caller]
fn assert_fmt(input: &str, expected: &str) {
    let got = format_source(input).unwrap_or_else(|d| panic!("refused: {d:?}"));
    assert_eq!(
        got, expected,
        "\n--- got ---\n{got}\n--- expected ---\n{expected}"
    );
    if let Err(msg) = common::check(input) {
        panic!("{msg}");
    }
}

#[test]
fn short_elements_stay_on_one_line() {
    assert_fmt(
        "const a = <br/>;\nconst b = <div></div>;\nconst c = <p>short</p>;\nconst d = <>frag</>;\n",
        "const a = <br />;\nconst b = <div></div>;\nconst c = <p>short</p>;\nconst d = <>frag</>;\n",
    );
}

#[test]
fn nested_elements_break_inside_parentheses() {
    assert_fmt(
        "function App() { return <div class=\"app\"><h1>Hello, {name}!</h1><p>Some text here</p></div>; }",
        "function App() {
  return (
    <div class=\"app\">
      <h1>Hello, {name}!</h1>
      <p>Some text here</p>
    </div>
  );
}
",
    );
}

#[test]
fn long_attribute_lists_break_one_per_line() {
    assert_fmt(
        "function f() { return <input type='text' name=\"username\" placeholder=\"Enter your user name here please\" disabled value={value} />; }",
        "function f() {
  return (
    <input
      type=\"text\"
      name=\"username\"
      placeholder=\"Enter your user name here please\"
      disabled
      value={value}
    />
  );
}
",
    );
}

#[test]
fn text_reflows_like_a_paragraph() {
    assert_fmt(
        "const x = <p>Lorem ipsum dolor sit amet, consectetur adipiscing elit, sed do eiusmod tempor incididunt ut labore et dolore magna aliqua. Ut enim ad minim veniam.</p>;",
        "const x = (
  <p>
    Lorem ipsum dolor sit amet, consectetur adipiscing elit, sed do eiusmod tempor incididunt ut
    labore et dolore magna aliqua. Ut enim ad minim veniam.
  </p>
);
",
    );
}

#[test]
fn spaces_next_to_tags_become_containers_where_lines_break() {
    assert_fmt(
        "const y = <p>Hello <b>world</b>, how are <i>you</i> today?</p>;",
        "const y = (
  <p>
    Hello <b>world</b>, how are <i>you</i> today?
  </p>
);
",
    );
    // Prettier measures a word without the `{" "}` after it, so this line is 102 columns.
    assert_fmt(
        "const t = <p>This is a long paragraph with an <a href=\"https://example.com/a/very/long/link\">inline link</a> and some <em>emphasis</em> right in the middle of it, isn't it?</p>;",
        "const t = (
  <p>
    This is a long paragraph with an <a href=\"https://example.com/a/very/long/link\">inline link</a>{\" \"}
    and some <em>emphasis</em> right in the middle of it, isn't it?
  </p>
);
",
    );
    assert_fmt(
        "const s2 = <p> <b>x</b> </p>;
const s3 = <p>	x	</p>;
const s4 = <p>{\" \"}leading and trailing{\" \"}</p>;
",
        "const s2 = (
  <p>
    {\" \"}
    <b>x</b>{\" \"}
  </p>
);
const s3 = <p> x </p>;
const s4 = <p> leading and trailing </p>;
",
    );
}

#[test]
fn several_spaces_are_kept() {
    // Prettier would print `<pre> a b </pre>`, which changes the text.
    assert_fmt(
        "const s = <pre>  a   b  </pre>;
",
        "const s = <pre>  a   b  </pre>;
",
    );
    assert_fmt(
        "const f = <p>  two leading spaces then a long text that has to break somewhere around here, <b>ok</b>?</p>;",
        "const f = (
  <p>
    {\"  \"}
    two leading spaces then a long text that has to break somewhere around here, <b>ok</b>?
  </p>
);
",
    );
}

#[test]
fn line_breaks_and_blank_lines_between_children() {
    assert_fmt(
        "const a = <div>

  <A />

  <B />
  <C />


  <D />
</div>;
const b = <div>
  text here

  <A />
  more text
</div>;
",
        "const a = (
  <div>
    <A />

    <B />
    <C />

    <D />
  </div>
);
const b = (
  <div>
    text here
    <A />
    more text
  </div>
);
",
    );
    assert_fmt(
        "const c = <p>text<br />more text that is quite long and goes past the limit of the line width for sure yes</p>;
const k = <div>x<input />yyyyyyyyy</div>;
const h = <div>{a}{b}</div>;
",
        "const c = (
  <p>
    text
    <br />
    more text that is quite long and goes past the limit of the line width for sure yes
  </p>
);
const k = (
  <div>
    x<input />
    yyyyyyyyy
  </div>
);
const h = (
  <div>
    {a}
    {b}
  </div>
);
",
    );
}

#[test]
fn long_containers_get_spaces_as_containers() {
    assert_fmt(
        "const a7 = <p>{aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa} {bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb}</p>;
const q = <Trans>Hello <b>{name}</b>, you have {count} new messages and {other} things waiting for you here</Trans>;
",
        "const a7 = (
  <p>
    {aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa}{\" \"}
    {bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb}
  </p>
);
const q = (
  <Trans>
    Hello <b>{name}</b>, you have {count} new messages and {other} things waiting for you here
  </Trans>
);
",
    );
}

#[test]
fn callbacks_returning_jsx_are_hugged() {
    assert_fmt(
        "const list = <main class=\"container\" id=\"main\">{posts.map((p) => <article key={p.id}><h2>{p.title}</h2><p>{p.excerpt}</p></article>)}</main>;",
        "const list = (
  <main class=\"container\" id=\"main\">
    {posts.map((p) => (
      <article key={p.id}>
        <h2>{p.title}</h2>
        <p>{p.excerpt}</p>
      </article>
    ))}
  </main>
);
",
    );
}

#[test]
fn elements_returned_by_callbacks_inside_braces_always_break() {
    assert_fmt(
        "const l = <ul>{items.map((i) => <li>{i}</li>)}</ul>;
const m = <ul class={f((i) => <b />)}>x</ul>;
",
        "const l = (
  <ul>
    {items.map((i) => (
      <li>{i}</li>
    ))}
  </ul>
);
const m = (
  <ul
    class={f((i) => (
      <b />
    ))}
  >
    x
  </ul>
);
",
    );
}

#[test]
fn logical_and_conditional_operands() {
    assert_fmt(
        "function g() { return ok && <div><span>yes</span></div>; }",
        "function g() {
  return ok && (
    <div>
      <span>yes</span>
    </div>
  );
}
",
    );
    assert_fmt(
        "const u = cond ? <div><a href=\"/x\">x</a></div> : null;",
        "const u = cond ? (
  <div>
    <a href=\"/x\">x</a>
  </div>
) : null;
",
    );
}

#[test]
fn conditionals_in_jsx_mode_wrap_every_branch() {
    assert_fmt(
        "function A() { return cond ? <div><span>a</span></div> : <p>b</p>; }",
        "function A() {
  return cond ? (
    <div>
      <span>a</span>
    </div>
  ) : (
    <p>b</p>
  );
}
",
    );
    assert_fmt(
        "const b2 = cond ? <a /> : <b />;\nconst b5 = <div>{cond ? <span>a</span> : null}</div>;\n",
        "const b2 = cond ? <a /> : <b />;\nconst b5 = <div>{cond ? <span>a</span> : null}</div>;\n",
    );
    assert_fmt(
        "const b4 = <div>{cond ? <span>aaaaaaaaaaaaaaaaaaaaaaaaaaaaaa</span> : <span>bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb</span>}</div>;",
        "const b4 = (
  <div>
    {cond ? (
      <span>aaaaaaaaaaaaaaaaaaaaaaaaaaaaaa</span>
    ) : (
      <span>bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb</span>
    )}
  </div>
);
",
    );
    // Prettier also wraps `"fallback string"` in parentheses; they would be a new `Paren` node.
    assert_fmt(
        "const b6 = veryLongConditionExpressionNameHere ? <span>aaaaaaaaaaaaaaaaaaaaaaa</span> : \"fallback string\";
const b8 = veryLongConditionExpressionNameHere ? <span>aaaaaaaaaaaaaaaaaaaaaaaaaa</span> : (\"written\");
",
        "const b6 = veryLongConditionExpressionNameHere ? (
  <span>aaaaaaaaaaaaaaaaaaaaaaa</span>
) : \"fallback string\";
const b8 = veryLongConditionExpressionNameHere ? (
  <span>aaaaaaaaaaaaaaaaaaaaaaaaaa</span>
) : (\"written\");
",
    );
}

#[test]
fn conditional_chains_with_an_element_break_together() {
    assert_fmt(
        "const b7 = a ? <x/> : b ? <y/> : <zzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzz />;",
        "const b7 = a ? (
  <x />
) : b ? (
  <y />
) : (
  <zzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzz />
);
",
    );
    // The element sits in a nested conditional: the whole chain is in JSX mode. Branches that
    // are not elements are not wrapped in parentheses (see above); they move to their own line
    // only when they do not fit.
    assert_fmt(
        "const c = first ? \"a string value that is long\" : second ? \"another long string value\" : <Fallback />;
const d = firstCondition ? \"a string value that is quite long\" : secondCondition ? \"another long string value\" : <Fallback />;
",
        "const c = first ? \"a string value that is long\" : second ? \"another long string value\" : (
  <Fallback />
);
const d = firstCondition ? \"a string value that is quite long\" : secondCondition ?
  \"another long string value\" : (
  <Fallback />
);
",
    );
}

#[test]
fn elements_as_statements_after_blocks() {
    assert_fmt(
        "function f(x: boolean) {\n  if (x) {\n  }\n  <div />;\n  while (x) {}\n  <p>a</p>;\n}\n",
        "function f(x: boolean) {\n  if (x) {}\n  <div />;\n  while (x) {}\n  <p>a</p>;\n}\n",
    );
}

#[test]
fn arrow_bodies_and_object_values() {
    assert_fmt(
        "const Item = (props: Props) => <li class=\"item\">{props.name} is a very long name that goes on and on</li>;",
        "const Item = (props: Props) => (
  <li class=\"item\">{props.name} is a very long name that goes on and on</li>
);
",
    );
    assert_fmt(
        "const o = { header: <Header title=\"x\" />, footer: <footer><p>bye</p></footer> };",
        "const o = {
  header: <Header title=\"x\" />,
  footer: (
    <footer>
      <p>bye</p>
    </footer>
  ),
};
",
    );
    assert_fmt(
        "const r = render(<App user={user} />, root);",
        "const r = render(<App user={user} />, root);\n",
    );
}

#[test]
fn expression_containers() {
    assert_fmt(
        "const q = <p>{veryLongVariableNameNumberOne + veryLongVariableNameNumberTwo + veryLongVariableNameNumberThree}</p>;",
        "const q = (
  <p>
    {veryLongVariableNameNumberOne +
      veryLongVariableNameNumberTwo +
      veryLongVariableNameNumberThree}
  </p>
);
",
    );
    assert_fmt(
        "const w = <A {...props} x=<b /> y:z='1' data-x-y=\"2\">{...kids}</A>;",
        "const w = (
  <A {...props} x=<b /> y:z=\"1\" data-x-y=\"2\">
    {...kids}
  </A>
);
",
    );
}

#[test]
fn comments_inside_jsx() {
    assert_fmt(
        "const a = <p>{/* one */ /* two */}</p>;\nconst b = <p>{x /* after */}</p>;\nconst c = <div>\n  {/* first */}\n  {// line comment\n  }\n</div>;\n",
        "const a = <p>{/* one */ /* two */}</p>;
const b = <p>{x /* after */}</p>;
const c = (
  <div>
    {/* first */}
    {// line comment
    }
  </div>
);
",
    );
    assert_fmt(
        "const d = <div /* in tag */></div /* closing */>;\nconst e = <br // trailing\n/>;\n",
        "const d = <div /* in tag */ /* closing */></div>;
const e = (
  <br
    // trailing
  />
);
",
    );
    // `//` in text is not a comment.
    assert_fmt(
        "const l = <a href='http://x'>see http://x // here</a>;",
        "const l = <a href=\"http://x\">see http://x // here</a>;\n",
    );
}

#[test]
fn entities_quotes_and_names_are_kept() {
    assert_fmt(
        "const g = <p>a&nbsp;b &#123; &lt;tag&gt; &amp;&amp; \"quotes\" 'single'</p>;\nconst h = <a title='He said \"hi\"' alt='x' />;\nconst f = <svg:rect xlink:href=\"#a\" />;\nconst o = <Card.Header>{c}</Card.Header>;\n",
        "const g = <p>a&nbsp;b &#123; &lt;tag&gt; &amp;&amp; \"quotes\" 'single'</p>;
const h = <a title='He said \"hi\"' alt=\"x\" />;
const f = <svg:rect xlink:href=\"#a\" />;
const o = <Card.Header>{c}</Card.Header>;
",
    );
}

#[test]
fn written_space_containers_are_kept() {
    assert_fmt(
        "const e = <p>{\"  \"}x{\" \"}<b>y</b></p>;",
        "const e = (
  <p>
    {\"  \"}x <b>y</b>
  </p>
);
",
    );
}

#[test]
fn generic_arrows_drop_the_tsx_comma() {
    assert_fmt(
        "const id = <T,>(x: T) => x;\nconst id2 = <T extends Show,>(x: T) => x;\nconst p = <A, B>(a: A, b: B) => a;\nconst q = async <T>(x: T) => <p>{x}</p>;\n",
        "const id = <T>(x: T) => x;\nconst id2 = <T extends Show>(x: T) => x;\nconst p = <A, B>(a: A, b: B) => a;\nconst q = async <T>(x: T) => <p>{x}</p>;\n",
    );
}

#[test]
fn layout_is_stable_under_reformatting() {
    let src = "function Page() {
  return <html><head><title>{title}</title></head><body>{posts.length > 0 ? <ul>{posts.map((p) => <li key={p.id}><a href={p.url}>{p.title}</a> by {p.author}</li>)}</ul> : <p>No posts yet, come back later when there is something to read here.</p>}</body></html>;
}
";
    common::check(src).unwrap_or_else(|m| panic!("{m}"));
    let once = format_source(src).unwrap();
    let narrow = once.replace("  ", "\t");
    common::check(&narrow).unwrap_or_else(|m| panic!("{m}"));
}
