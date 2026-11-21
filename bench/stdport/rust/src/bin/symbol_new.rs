// `Symbol(description)`: the same program as bench/stdport/symbol_new.velt, with a leaked record
// per symbol sharing its description (an `Rc<str>`), as the runtime's records did.
use std::hint::black_box;
use std::rc::Rc;

struct Rec {
    desc: Option<Rc<str>>,
}

fn main() {
    let words: Vec<Rc<str>> = ["k", "user:session", "a-longer-description-than-fits-inline"]
        .iter()
        .map(|w| Rc::from(*w))
        .collect();
    let mut made: Vec<&'static Rec> = Vec::new();
    for i in 0..400_000usize {
        let desc = (i % 7 != 0).then(|| words[i % 3].clone());
        made.push(Box::leak(Box::new(Rec { desc })));
    }
    let mut total = 0usize;
    for _ in 0..2 {
        for s in &made {
            let s = black_box(*s);
            let d = s.desc.clone();
            let text = format!("Symbol({})", s.desc.as_deref().unwrap_or(""));
            total += d.map_or(0, |d| d.chars().map(char::len_utf16).sum()) + text.len();
        }
    }
    println!("{total}");
}
