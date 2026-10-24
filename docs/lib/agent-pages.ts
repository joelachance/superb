import { source } from '@/lib/source';

/** Pages an agent should read: the task index, generated references, rules and contracts. */
const ORDER = [
  'quick-start',
  'architecture',
  'terminology',
  'flows/session.ref',
  'flows/track.ref',
  'flows/clip.ref',
  'flows/marker.ref',
  'flows/transport.ref',
  'flows/send.ref',
  'flows/midi.ref',
  'flows/record.ref',
  'flows/metronome.ref',
  'flows/export.ref',
  'flows/devices.ref',
  'primitives',
  'primitives/buffers',
  'primitives/realtime',
  'primitives/transport',
  'primitives/midi',
  'primitives/graph',
  'primitives/dynamics',
  'primitives/mixer',
  'primitives/automation',
  'primitives/analysis',
  'primitives/undo',
  'primitives/session',
  'primitives/plugins',
  'primitives/io',
];

export function agentPages() {
  const bySlug = new Map(source.getPages().map((page) => [page.slugs.join('/'), page]));
  return ORDER.flatMap((slug) => {
    const page = bySlug.get(slug);
    return page ? [page] : [];
  });
}
