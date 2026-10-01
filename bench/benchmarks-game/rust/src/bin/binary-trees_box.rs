// binary-trees with `Box` nodes on mimalloc, single-threaded. NOT a published Benchmarks Game
// program: every published Rust binary-trees uses an arena crate. It exists for a same-allocator
// comparison with the idiomatic Velt port (bench/benchmarks-game/binary-trees/main.vlt), which
// allocates and frees every node individually through the Velt runtime's mimalloc.
// Derived from Rust #5 (the Rust Project Developers, TeXitoi, Cristi Cobzarenco, Matt Brubeck;
// modified by Tom Kaitchuck, Volodymyr M. Lisivka and Ryohei Machida): bumpalo → `Box`, rayon →
// plain loops.

#[global_allocator]
static GLOBAL: mimalloc::MiMalloc = mimalloc::MiMalloc;

struct Tree {
    left: Option<Box<Tree>>,
    right: Option<Box<Tree>>,
}

fn item_check(tree: &Tree) -> i32 {
    if let (Some(left), Some(right)) = (&tree.left, &tree.right) {
        1 + item_check(right) + item_check(left)
    } else {
        1
    }
}

fn bottom_up_tree(depth: i32) -> Box<Tree> {
    let mut tree = Box::new(Tree { left: None, right: None });
    if depth > 0 {
        tree.right = Some(bottom_up_tree(depth - 1));
        tree.left = Some(bottom_up_tree(depth - 1));
    }
    tree
}

fn inner(depth: i32, iterations: i32) -> String {
    let chk: i32 = (0..iterations)
        .map(|_| {
            let a = bottom_up_tree(depth);
            item_check(&a)
        })
        .sum();
    format!("{}\t trees of depth {}\t check: {}", iterations, depth, chk)
}

fn main() {
    let n = std::env::args().nth(1).and_then(|n| n.parse().ok()).unwrap_or(10);
    let min_depth = 4;
    let max_depth = if min_depth + 2 > n { min_depth + 2 } else { n };

    {
        let depth = max_depth + 1;
        let tree = bottom_up_tree(depth);
        println!(
            "stretch tree of depth {}\t check: {}",
            depth,
            item_check(&tree)
        );
    }

    let long_lived_tree = bottom_up_tree(max_depth);

    for half_depth in min_depth / 2..=max_depth / 2 {
        let depth = half_depth * 2;
        let iterations = 1 << ((max_depth - depth + min_depth) as u32);
        println!("{}", inner(depth, iterations));
    }

    println!(
        "long lived tree of depth {}\t check: {}",
        max_depth,
        item_check(&long_lived_tree)
    );
}
