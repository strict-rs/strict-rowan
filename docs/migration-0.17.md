# Migrating to Rowan 0.17

Rowan 0.17 replaces its locally unsafe green-tree allocation and mutable red-cursor machinery with safe immutable shared handles, fallible construction and query contracts, and an exclusively borrowed transactional editor. This is an intentional breaking release: there are no compatibility aliases, deprecated panic wrappers, or parallel legacy APIs.

## API replacements

| Rowan 0.16 or earlier | Rowan 0.17 |
|---|---|
| `&GreenNodeData` | `&GreenNode` |
| `&GreenTokenData` | `&GreenToken` |
| `SyntaxNode::green() -> Cow<GreenNodeData>` | `SyntaxNode::green() -> &GreenNode` |
| Infallible `GreenNode::new` and `GreenToken::new` | Propagate `Result<_, GreenError>` |
| Infallible builder mutation | Propagate `BuildError` with `?` |
| `start_node_at(checkpoint, kind)` | `start_node_at(&checkpoint, kind)` |
| `new_root_mut`, `clone_for_update`, red `detach`, and red `splice_children` | Construct `SyntaxEditor`, edit by stable IDs, then call `into_syntax` |
| Panicking query preconditions | Handle `RangeError` |
| `SyntaxNodePtr::to_node` and `SyntaxNodePtr::try_to_node` | `SyntaxNodePtr::resolve` |
| `AstPtr::to_node` and `AstPtr::try_to_node` | `AstPtr::resolve` |
| Infallible `AstNode::clone_subtree` | `Result<Self, AstError>` |
| Public raw `to_next_sibling*` | No public replacement; use ordinary borrowed navigation. Consuming allocation reuse is now private. |

## Green construction

Green nodes and tokens are still immutable and structurally shared. Their constructors now validate text-coordinate limits and expose allocation failure through `GreenError`:

```rust
use rowan::{GreenElement, GreenNode, GreenToken, SyntaxKind};

# fn build() -> Result<GreenNode, rowan::GreenError> {
let identifier = GreenToken::new(SyntaxKind(1), "name")?;
let root = GreenNode::new(SyntaxKind(0), [GreenElement::from(identifier)])?;
# Ok(root)
# }
```

`GreenNode::children` now yields `GreenElementRef<'_>`, whose node/token values are borrowed `&GreenNode` and `&GreenToken` handles. Call `GreenElementRef::to_owned` when an owned shared handle is required.

## Builder failures and checkpoints

`GreenNodeBuilder::token`, `finish_node`, `start_node_at`, and `finish` return `Result`. A rejected operation leaves the reusable builder state unchanged. Checkpoints are reusable opaque values, but are no longer `Copy`; pass them by reference:

```rust
use rowan::{GreenNodeBuilder, SyntaxKind};

# fn build() -> Result<rowan::GreenNode, rowan::BuildError> {
let mut builder = GreenNodeBuilder::new();
builder.start_node(SyntaxKind(0));
let checkpoint = builder.checkpoint();
builder.token(SyntaxKind(1), "value")?;
builder.start_node_at(&checkpoint, SyntaxKind(2))?;
builder.finish_node()?;
builder.finish_node()?;
builder.finish()
# }
```

Handle `BuildError` as a protocol or construction failure. Parser diagnostics such as an unmatched delimiter remain successful parser outcomes in the parser's own diagnostic collection.

## Immutable queries and text

Cursor offsets are absolute, while `SyntaxText` offsets are relative to the selected text view. Cursor range queries now return `RangeError`. `SyntaxText::slice`, `char_at`, `contains_char`, `find_char`, and chunk visitors validate range and UTF-8 boundaries rather than panicking.

Chunk callbacks distinguish Rowan validation from caller failure:

```rust
# use rowan::{ChunkError, SyntaxText};
# fn visit(text: &SyntaxText) -> Result<(), ChunkError<std::convert::Infallible>> {
text.try_for_each_chunk(|chunk| {
  let _byte_len = chunk.len();
  Ok::<(), std::convert::Infallible>(())
})
# }
```

Preorder `skip_subtree` also returns `TraversalError`. A skip is valid only immediately after the iterator yields `WalkEvent::Enter(Node(_))`.

## Transactional editing

Mutable red cursors have been removed. `SyntaxEditor` materializes one source subtree into an editor-owned arena and addresses elements with stable, opaque IDs. IDs remain valid across detach, move, and reinsertion operations, but an ID from one editor is rejected by another.

```rust
use rowan::{Language, SyntaxEditor, SyntaxNode};

# fn remove_first_child<L: Language>(root: &SyntaxNode<L>) -> Result<SyntaxNode<L>, rowan::EditError> {
let mut editor = SyntaxEditor::new(root);
let root_id = editor.root_id();
if let Some(first) = editor.children(&root_id)?.first().cloned() {
  let _detached = editor.detach(&first)?;
}
editor.into_syntax(&root_id)
# }
```

`splice_children` validates ownership, ranges, duplicate insertions, cycles, text lengths, offsets, and all derived metadata before committing. Every error leaves the editor unchanged. Removed elements are returned by `SpliceOutcome`; moved insertion elements are not reported as detached because they remain connected at the destination.

`into_syntax` accepts only a detached node component. It reuses unchanged green allocations and rebuilds only dirty ancestor paths. The immutable source passed to `SyntaxEditor::new` is never modified.

## AST pointers

Use `SyntaxNodePtr::resolve` or `AstPtr::resolve` and handle `ResolveError::RootHasParent` when the supplied cursor is not a root. A missing, wrong-kind, or non-equivalent tree location returns `Ok(None)`. `AstNode::clone_subtree` now reports a faulty wrapper whose `cast` rejects its own cloned syntax as `AstError::CloneCastRejected`.
