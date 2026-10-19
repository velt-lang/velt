struct TreeNode { value: i64, children: Vec<TreeNode> }
fn build(depth: i64, width: i64, seed: i64) -> TreeNode {
    let mut node = TreeNode { value: seed, children: Vec::new() };
    if depth > 0 { for i in 0..width { node.children.push(build(depth - 1, width, seed * width + i)); } }
    node
}
fn sum(node: &TreeNode) -> i64 { node.value % 7 + node.children.iter().map(sum).sum::<i64>() }
fn main() {
    let mut total = 0;
    for round in 0..60 { let tree = build(7, 4, round); total += sum(&tree); }
    println!("{}", total);
}
