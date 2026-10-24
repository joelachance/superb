import { createMDX } from 'fumadocs-mdx/next';

const withMDX = createMDX();

/** @type {import('next').NextConfig} */
const config = {
  reactStrictMode: true,
  // `next dev` would otherwise write an AGENTS.md into docs/.
  agentRules: false,
};

export default withMDX(config);
