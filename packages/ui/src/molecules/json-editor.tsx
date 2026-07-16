"use client";

import * as React from "react";

import { cn } from "../lib/utils";

interface JsonEditorProps extends React.TextareaHTMLAttributes<HTMLTextAreaElement> {
  error?: string;
}

export const JsonEditor = React.forwardRef<HTMLTextAreaElement, JsonEditorProps>(
  ({ className, error, ...props }, ref) => {
    return (
      <div className="grid gap-2">
        <textarea
          ref={ref}
          className={cn(
            "flex min-h-[320px] w-full rounded-md border border-input bg-background px-3 py-2 font-mono text-sm ring-offset-background placeholder:text-muted-foreground focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring focus-visible:ring-offset-2 disabled:cursor-not-allowed disabled:opacity-50",
            error && "border-destructive",
            className,
          )}
          spellCheck={false}
          {...props}
        />
        {error && <p className="text-sm text-destructive">{error}</p>}
      </div>
    );
  },
);
JsonEditor.displayName = "JsonEditor";
