import { docsLlms } from '@/lib/source';
import { agentPages } from '@/lib/agent-pages';

export const revalidate = false;

export async function GET() {
  const parts: string[] = [];
  for (const page of agentPages()) parts.push(await docsLlms.page(page));
  return new Response(parts.join('\n\n'));
}
