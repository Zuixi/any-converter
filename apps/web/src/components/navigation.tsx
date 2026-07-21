"use client";

import Link from "next/link";
import { usePathname } from "next/navigation";

import { useI18n } from "@any-converter/core";
import { cn } from "@any-converter/ui";

const navItems = [
  { href: "/playground", label: "nav.playground" },
  { href: "/logs", label: "nav.logs" },
  { href: "/usage", label: "nav.usage" },
  { href: "/status", label: "nav.status" },
  { href: "/config", label: "nav.config" },
] as const;

export function Navigation() {
  const { language, setLanguage, t } = useI18n();
  const pathname = usePathname();

  return (
    <header className="sticky top-0 z-50 border-b bg-background/95 backdrop-blur">
      <div className="container flex h-14 items-center">
        <Link href="/" className="mr-6 text-lg font-bold">
          any-converter
        </Link>
        <nav className="flex items-center gap-4 text-sm">
          {navItems.map((item) => (
            <Link
              key={item.href}
              href={item.href}
              className={cn(
                "transition-colors hover:text-foreground/80",
                pathname === item.href ? "text-foreground font-medium" : "text-muted-foreground",
              )}
            >
              {t(item.label)}
            </Link>
          ))}
        </nav>
        <button
          type="button"
          className="ml-auto rounded-md border px-2 py-1 text-xs text-muted-foreground hover:text-foreground"
          onClick={() => setLanguage(language === "en" ? "zh-CN" : "en")}
        >
          {language === "en" ? t("common.chinese") : t("common.english")}
        </button>
      </div>
    </header>
  );
}
