export function PlaceholderPage({
  title,
  desc,
  hint,
}: {
  title: string;
  desc: string;
  hint: string;
}) {
  return (
    <div className="h-full flex flex-col items-center justify-center text-center px-8">
      <div className="card px-10 py-8 max-w-md">
        <h1 className="text-[20px] font-bold tracking-tight">{title}</h1>
        <p className="text-[13px] text-[var(--text-2)] mt-2 leading-relaxed">{desc}</p>
        <div className="mt-5 inline-flex rounded-full bg-[var(--accent)]/10 text-[var(--accent)] px-3 py-1 text-[12px] font-medium">
          {hint}
        </div>
      </div>
    </div>
  );
}
