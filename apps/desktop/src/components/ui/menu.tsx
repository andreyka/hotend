// Menu: project name, Views (DESIGN_SPEC 8), on Base UI's Menu.
import { Menu as MenuPrimitive } from "@base-ui/react/menu";

import { cn } from "@/lib/utils";

const Menu = MenuPrimitive.Root;
const MenuTrigger = MenuPrimitive.Trigger;

function MenuContent({
  className,
  side = "bottom",
  align = "start",
  ...props
}: MenuPrimitive.Popup.Props & Pick<MenuPrimitive.Positioner.Props, "side" | "align">) {
  return (
    <MenuPrimitive.Portal>
      <MenuPrimitive.Positioner side={side} align={align} sideOffset={6} className="z-40">
        <MenuPrimitive.Popup className={cn("app-popup min-w-[176px] p-1", className)} {...props} />
      </MenuPrimitive.Positioner>
    </MenuPrimitive.Portal>
  );
}

function MenuItem({ className, ...props }: MenuPrimitive.Item.Props) {
  return (
    <MenuPrimitive.Item
      className={cn(
        "t-ui flex h-7 cursor-default items-center gap-2 rounded-tag px-2 select-none data-[highlighted]:bg-app-hover data-[disabled]:opacity-40 [&_svg]:size-4 [&_svg]:shrink-0",
        className,
      )}
      {...props}
    />
  );
}

export { Menu, MenuTrigger, MenuContent, MenuItem };
