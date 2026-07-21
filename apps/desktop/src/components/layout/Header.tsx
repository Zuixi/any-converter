export function Header({ title, subtitle }: { title: string; subtitle: string }) {
  return (
    <div className="grid gap-2">
      <h1 className="text-3xl font-bold">{title}</h1>
      <p className="text-muted-foreground">{subtitle}</p>
    </div>
  );
}
