# Folder dependency cycles

These cycles belong to the final folder graph, after root filtering and depth folding. Folder aggregation can introduce cycles even when the file graph is acyclic.

## C1 (#E41A1C)

- <code>src</code> -> <code>src</code>

## C2 (#377EB8)

- <code>src/export</code> -> <code>src/export</code>

## C3 (#4DAF4A)

- <code>src/graph</code> -> <code>src/graph</code>
