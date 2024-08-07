class Node {
  constructor(name, parent) {
    this.name = name;
    this.parent = parent;
  }
  get path() {
    const path = [];
    let node = this;
    while (node) {
      path.push(node.name);
      node = node.parent;
    }
    return path.reverse().join('/');
  }
  get isRoot() {
    return !this.parent;
  }
}
export default Node;
