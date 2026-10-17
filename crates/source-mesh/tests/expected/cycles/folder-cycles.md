# Folder dependency cycles

These cycles belong to the final folder graph, after root filtering and depth folding. Folder aggregation can introduce cycles even when the file graph is acyclic.

## C1 (#E41A1C)

- <code>crates/source-mesh-core/src</code> -> <code>crates/source-mesh-core/src</code>

## C2 (#377EB8)

- <code>crates/source-mesh-core/src/export</code> -> <code>crates/source-mesh-core/src/export</code>

## C3 (#4DAF4A)

- <code>crates/source-mesh-core/src/export/dot</code> -> <code>crates/source-mesh-core/src/export/dot</code>

## C4 (#984EA3)

- <code>crates/source-mesh-core/src/export/graphml</code> -> <code>crates/source-mesh-core/src/export/graphml</code>

## C5 (#FF7F00)

- <code>crates/source-mesh-core/src/export/mermaid</code> -> <code>crates/source-mesh-core/src/export/mermaid</code>

## C6 (#FFFF33)

- <code>crates/source-mesh-core/src/export/rendering</code> -> <code>crates/source-mesh-core/src/export/rendering</code>

## C7 (#A65628)

- <code>crates/source-mesh-core/src/graph</code> -> <code>crates/source-mesh-core/src/graph</code>

## C8 (#F781BF)

- <code>crates/source-mesh/src</code> -> <code>crates/source-mesh/src</code>
