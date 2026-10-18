# Folder dependency cycles

These cycles belong to the final folder graph, after root filtering and depth folding. Folder aggregation can introduce cycles even when the file graph is acyclic.

## C1 (#6D3FA9)

- <code>crates/source-mesh-core/src</code> -> <code>crates/source-mesh-core/src</code>

## C2 (#BF3CAE)

- <code>crates/source-mesh-core/src/analyzer</code> -> <code>crates/source-mesh-core/src/analyzer</code>

## C3 (#FE4B82)

- <code>crates/source-mesh-core/src/diagnostics</code> -> <code>crates/source-mesh-core/src/diagnostics</code>

## C4 (#FF7746)

- <code>crates/source-mesh-core/src/export</code> -> <code>crates/source-mesh-core/src/export</code>

## C5 (#E2B72E)

- <code>crates/source-mesh-core/src/export/dot</code> -> <code>crates/source-mesh-core/src/export/dot</code>

## C6 (#AFEF5A)

- <code>crates/source-mesh-core/src/export/graphml</code> -> <code>crates/source-mesh-core/src/export/graphml</code>

## C7 (#52F566)

- <code>crates/source-mesh-core/src/export/mermaid</code> -> <code>crates/source-mesh-core/src/export/mermaid</code>

## C8 (#1DDEA3)

- <code>crates/source-mesh-core/src/export/rendering</code> -> <code>crates/source-mesh-core/src/export/rendering</code>

## C9 (#23AAD8)

- <code>crates/source-mesh-core/src/graph</code> -> <code>crates/source-mesh-core/src/graph</code>

## C10 (#4C6EDB)

- <code>crates/source-mesh-core/src/scip</code> -> <code>crates/source-mesh-core/src/scip</code>
