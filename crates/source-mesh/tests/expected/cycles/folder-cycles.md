# Folder dependency cycles

These cycles belong to the final folder graph, after root filtering and depth folding. Folder aggregation can introduce cycles even when the file graph is acyclic.

## C1 (#6D3FA9)

- <code>crates/source-mesh-core/src</code> -> <code>crates/source-mesh-core/src</code>

## C2 (#B73CB0)

- <code>crates/source-mesh-core/src/analyzer</code> -> <code>crates/source-mesh-core/src/analyzer</code>

## C3 (#F5468D)

- <code>crates/source-mesh-core/src/diagnostics</code> -> <code>crates/source-mesh-core/src/diagnostics</code>

## C4 (#FF6855)

- <code>crates/source-mesh-core/src/export</code> -> <code>crates/source-mesh-core/src/export</code>

## C5 (#F49F30)

- <code>crates/source-mesh-core/src/export/dot</code> -> <code>crates/source-mesh-core/src/export/dot</code>

## C6 (#C3D83E)

- <code>crates/source-mesh-core/src/export/graphml</code> -> <code>crates/source-mesh-core/src/export/graphml</code>

## C7 (#82F557)

- <code>crates/source-mesh-core/src/export/mermaid</code> -> <code>crates/source-mesh-core/src/export/mermaid</code>

## C8 (#38F179)

- <code>crates/source-mesh-core/src/export/rendering</code> -> <code>crates/source-mesh-core/src/export/rendering</code>

## C9 (#19D2B4)

- <code>crates/source-mesh-core/src/graph</code> -> <code>crates/source-mesh-core/src/graph</code>

## C10 (#299FDD)

- <code>crates/source-mesh-core/src/scip</code> -> <code>crates/source-mesh-core/src/scip</code>

## C11 (#5069D8)

- <code>crates/source-mesh/src</code> -> <code>crates/source-mesh/src</code>
