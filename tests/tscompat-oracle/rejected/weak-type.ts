// `weak T` is Velt's weak reference; `tsc` doesn't know the operator.

export class TreeNode {
  parent: weak TreeNode | null = null;
  children: TreeNode[] = [];
}
