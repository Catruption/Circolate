cmake_minimum_required(VERSION 3.20)
project(halotext_playhead VERSION 0.1.0 LANGUAGES CXX)

set(CMAKE_CXX_STANDARD 17)
set(CMAKE_CXX_STANDARD_REQUIRED ON)
set(CMAKE_AUTOMOC ON)

find_package(ECM REQUIRED NO_MODULE)
list(APPEND CMAKE_MODULE_PATH "${ECM_MODULE_PATH}")

include(KDEInstallDirs)
include(KDECMakeSettings)

# Some distro ECM builds leave these unset when built outside a KDE superbuild.
set(CMAKE_LIBRARY_OUTPUT_DIRECTORY "${CMAKE_BINARY_DIR}/lib")

find_package(Qt6 REQUIRED COMPONENTS Core Network)
find_package(KF6 REQUIRED COMPONENTS CoreAddons TextEditor)

kcoreaddons_add_plugin(halotext_playhead
    SOURCES plugin.cpp plugin.h
    INSTALL_NAMESPACE kf6/ktexteditor
)

target_link_libraries(halotext_playhead PRIVATE
    Qt6::Core
    Qt6::Network
    KF6::CoreAddons
    KF6::TextEditor
)
