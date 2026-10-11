import Link from 'next/link';

export default function HomePage() {
  return (
    <div className="flex flex-col justify-center items-center text-center flex-1 gap-6 px-4">
      <h1 className="text-4xl font-bold">Aura</h1>
      <p className="max-w-xl text-fd-muted-foreground">
        An open-source Rust toolkit for building a digital audio workstation.
        One crate, flows over primitives, no user interface: you bring your own.
      </p>
      <div className="flex gap-3">
        <Link href="/docs" className="rounded-md bg-fd-primary px-4 py-2 text-fd-primary-foreground font-medium">
          Read the docs
        </Link>
        <Link href="/docs/flows" className="rounded-md border px-4 py-2 font-medium">
          The flows
        </Link>
      </div>
    </div>
  );
}
