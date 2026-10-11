# Folder dependency cycles

These cycles belong to the final folder graph, after root filtering and depth folding. Folder aggregation can introduce cycles even when the file graph is acyclic.

## C1 (#E41A1C)

- <code>crates/source-mesh-core/src</code> -> <code>crates/source-mesh-core/src</code>

## C2 (#377EB8)

- <code>crates/source-mesh-core/src/export</code> -> <code>crates/source-mesh-core/src/export</code>

## C3 (#4DAF4A)

- <code>crates/source-mesh-core/src/graph</code> -> <code>crates/source-mesh-core/src/graph</code>
