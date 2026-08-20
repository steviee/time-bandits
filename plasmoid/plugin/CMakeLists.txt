# SPDX-FileCopyrightText: 2026 Time Bandits contributors
# SPDX-License-Identifier: GPL-3.0-or-later
#
# The one piece of the project that is not built by Cargo.
#
# Plasma 6 gives QML no way to speak D-Bus, and no data engines are installed on
# a modern Plasma system — every shipped KDE widget with external data carries a
# plugin like this. It needs Qt only: no KDE Frameworks, no Extra CMake Modules,
# which keeps the Plasma package's build dependencies to what any distribution
# already has for Qt.

cmake_minimum_required(VERSION 3.21)

project(timebandits-screentime-plugin VERSION 0.1.0 LANGUAGES CXX)

set(CMAKE_CXX_STANDARD 20)
set(CMAKE_CXX_STANDARD_REQUIRED ON)
set(CMAKE_AUTOMOC ON)

find_package(Qt6 6.6 REQUIRED COMPONENTS Core DBus Qml)

qt_standard_project_setup(REQUIRES 6.6)

# OUTPUT_DIRECTORY, so the generated qmldir and qmltypes land beside the
# plugin in a directory shaped like the URI. Without it Qt writes them flat
# into the build root, the install rules below cannot find them, and the module
# is unloadable — which is how this went unnoticed: no packaging recipe called
# install-plasma, so nothing ever tried.
set(MODULE_BUILD_DIR
    "${CMAKE_CURRENT_BINARY_DIR}/qml/org/timebandits/screentime/private")

qt_add_qml_module(timebandits_screentime_plugin
    URI org.timebandits.screentime.private
    VERSION 1.0
    PLUGIN_TARGET timebandits_screentime_plugin
    OUTPUT_DIRECTORY "${MODULE_BUILD_DIR}"
    SOURCES
        screentimeagent.cpp
        screentimeagent.h
)

target_link_libraries(timebandits_screentime_plugin
    PRIVATE Qt6::Core Qt6::DBus Qt6::Qml
)

# Where Qt looks for QML modules. Distributions disagree — lib64 on Fedora,
# a triplet path on Debian — so it is asked of Qt rather than guessed, and can
# still be overridden by the packaging.
if(NOT DEFINED QML_INSTALL_DIR)
    get_target_property(_qmake Qt6::qmake IMPORTED_LOCATION)
    execute_process(COMMAND "${_qmake}" -query QT_INSTALL_QML
                    OUTPUT_VARIABLE _qml_dir
                    OUTPUT_STRIP_TRAILING_WHITESPACE
                    ERROR_QUIET)
    if(_qml_dir)
        set(QML_INSTALL_DIR "${_qml_dir}")
    else()
        set(QML_INSTALL_DIR "${CMAKE_INSTALL_PREFIX}/lib/qt6/qml")
    endif()
endif()
message(STATUS "QML modules install to ${QML_INSTALL_DIR}")

set(MODULE_DIR "${QML_INSTALL_DIR}/org/timebandits/screentime/private")

install(TARGETS timebandits_screentime_plugin
        LIBRARY DESTINATION "${MODULE_DIR}"
        RUNTIME DESTINATION "${MODULE_DIR}")

# A plugin without its qmldir is not a module Qt can import.
install(FILES
            "${MODULE_BUILD_DIR}/qmldir"
            "${MODULE_BUILD_DIR}/timebandits_screentime_plugin.qmltypes"
        DESTINATION "${MODULE_DIR}")
