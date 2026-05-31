# MCP prompt discovery + usage

This example shows how to discover MCP prompts and fetch the prompt payload using
`@modelcontextprotocol/sdk`.

## Prerequisite
- `spark-mcp` running at `http://127.0.0.1:9410/mcp`.

## Example (Node.js)
```ts
import { Client } from '@modelcontextprotocol/sdk/client/index.js';
import { StreamableHTTPClientTransport } from '@modelcontextprotocol/sdk/client/streamableHttp.js';

const client = new Client({ name: 'spark-mcp-client', version: '0.0.0' });
const transport = new StreamableHTTPClientTransport(
  new URL('http://127.0.0.1:9410/mcp'),
);

await client.connect(transport);

const prompts = await client.listPrompts();
console.log('Prompts:', prompts.prompts.map((p) => p.name));

const prompt = await client.getPrompt({
  name: 'spark.grounded_answer_checklist',
  arguments: { question: 'How do I write SPARK loop invariants?' },
});
console.log(prompt.messages);

await transport.close();
```

## Expected prompt names
- `spark.grounded_answer`
- `spark.grounded_answer_checklist`
