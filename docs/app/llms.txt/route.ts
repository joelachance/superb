import { agentPages } from '@/lib/agent-pages';

export const revalidate = false;

export async function GET() {
  const lines = [
    '# Superb',
    '',
    'Task index, generated flow references, architecture, terminology and primitive contracts.',
    'The short flow pages (examples, no signatures) are linked from each reference.',
    '',
  ];
  for (const page of agentPages()) {
    const description = page.data.description ? `: ${page.data.description}` : '';
    lines.push(`- [${page.data.title}](${page.url})${description}`);
  }
  return new Response(lines.join('\n') + '\n');
}
