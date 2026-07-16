import type { Metadata } from "next";

import { Navigation } from "@/components/navigation";
import "./globals.css";

export const metadata: Metadata = {
  title: "any-converter web",
  description: "Web interface for any-converter",
};

export default function RootLayout({ children }: { children: React.ReactNode }) {
  return (
    <html lang="en">
      <body className="min-h-screen bg-background font-sans antialiased">
        <Navigation />
        <main className="container py-6">{children}</main>
      </body>
    </html>
  );
}
