"""Install the embedded package once, without adding filesystem import paths."""

import sys
import types


def install(sources):
    package_name = "_fgdb_languages_v1"
    existing = sys.modules.get(package_name)

    if existing is not None:
        if not getattr(existing, "_installed", False):
            raise RuntimeError("The fgdb language package name is already in use")

        return

    package = types.ModuleType(package_name)
    package.__path__ = ()
    sys.modules[package_name] = package
    installed = [package_name]

    try:
        for name, source in sources:
            full_name = package_name + "." + name

            if full_name in sys.modules:
                raise RuntimeError("The fgdb language module name is already in use: " + name)

            module = types.ModuleType(full_name)
            module.__package__ = package_name
            sys.modules[full_name] = module
            installed.append(full_name)
            exec(compile(source, "<fgdb/" + name + ".py>", "exec"), module.__dict__)
            setattr(package, name, module)

        # Register only after every dependency is ready. Reloading preserves
        # GDB's enable/disable state and never duplicates a printer collection.
        package._printer = package.printers.register()
        package.returns.register()
        package._installed = True
    except BaseException:
        for name in reversed(installed):
            sys.modules.pop(name, None)

        raise


install(_sources)
